use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use amafu_cluster_address::{ADVERTISEMENT_PORT, AdvertisementKey, Claim, Election};
use amafu_domain::NodeName;
use amafu_domain::kinds::cluster_address::ClusterAddressSpecification;
use amafu_interfaces::ClusterAddressClaim;
use amafu_network::cluster_address::{hold_cluster_address, release_cluster_address};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::watch;

use super::settings::ClusterSettings;
use crate::dashboard::DASHBOARD_PORT;

const TICK: Duration = Duration::from_secs(1);
const STEP_ASIDE_DEADLINE: Duration = Duration::from_secs(3);
const STEP_ASIDE_POLL: Duration = Duration::from_millis(100);
const DASHBOARD_ANSWER_TIMEOUT: Duration = Duration::from_millis(500);
const LARGEST_ADVERTISEMENT_BYTES: usize = 1024;

#[derive(Default)]
pub struct AddressClaim {
    holds: AtomicBool,
    is_held_by_a_peer: AtomicBool,
    is_stepping_aside: AtomicBool,
}

#[async_trait::async_trait]
impl ClusterAddressClaim for AddressClaim {
    fn holds(&self) -> bool {
        self.holds.load(Ordering::Relaxed)
    }

    async fn step_aside(&self) {
        self.is_stepping_aside.store(true, Ordering::Relaxed);
        if !self.holds() {
            return;
        }
        let deadline = tokio::time::Instant::now() + STEP_ASIDE_DEADLINE;
        while tokio::time::Instant::now() < deadline {
            if !self.holds() && self.is_held_by_a_peer.load(Ordering::Relaxed) {
                tracing::info!("another node took the cluster address over");
                return;
            }
            tokio::time::sleep(STEP_ASIDE_POLL).await;
        }
        tracing::warn!("no other node took the cluster address over before the restart");
    }
}

async fn dashboard_answers() -> bool {
    let local_dashboard = SocketAddr::from((Ipv4Addr::LOCALHOST, DASHBOARD_PORT));
    matches!(
        tokio::time::timeout(DASHBOARD_ANSWER_TIMEOUT, TcpStream::connect(local_dashboard)).await,
        Ok(Ok(_))
    )
}

struct AddressOnInterface {
    held: Option<ClusterAddressSpecification>,
}

impl AddressOnInterface {
    async fn follow(&mut self, claim: Claim, wanted: Option<&ClusterAddressSpecification>) {
        let wanted = wanted.filter(|_| claim == Claim::Hold);
        if self.held.as_ref() == wanted {
            return;
        }
        if let Some(held) = self.held.take() {
            release(held.address).await;
        }
        let Some(wanted) = wanted else {
            return;
        };
        match hold_cluster_address(wanted).await {
            Ok(()) => self.held = Some(wanted.clone()),
            Err(error) => tracing::warn!(%error, "the cluster address cannot be held"),
        }
    }
}

async fn release(address: Ipv4Addr) {
    if let Err(error) = release_cluster_address(address).await {
        tracing::warn!(%error, %address, "the cluster address cannot be released");
    }
}

pub struct ElectionRole {
    pub me: NodeName,
    pub peers: Vec<IpAddr>,
    pub settings: watch::Receiver<ClusterSettings>,
    pub claim: Arc<AddressClaim>,
}

impl ElectionRole {
    pub async fn take_part(&self) -> String {
        let socket = match UdpSocket::bind((Ipv4Addr::UNSPECIFIED, ADVERTISEMENT_PORT)).await {
            Ok(socket) => socket,
            Err(error) => return format!("listening for advertisements failed: {error}"),
        };
        let stale = self.settings.borrow().address.as_ref().map(|address| address.address);
        if let Some(stale) = stale {
            release(stale).await;
        }
        let mut election = Election::new(self.me.clone(), Instant::now());
        let mut interface = AddressOnInterface { held: None };
        let mut ticks = tokio::time::interval(TICK);
        let mut received = [0_u8; LARGEST_ADVERTISEMENT_BYTES];
        loop {
            let key = self.advertisement_key();
            tokio::select! {
                _ = ticks.tick() => {
                    self.decide(&mut election, &mut interface).await;
                    if let Some(key) = &key {
                        self.advertise(&socket, key, &election).await;
                    }
                }
                Ok((length, _)) = socket.recv_from(&mut received) => {
                    if let Some(advertisement) = key.and_then(|key| key.open(&received[..length])) {
                        election.hear(advertisement, Instant::now());
                    }
                }
            }
        }
    }

    fn advertisement_key(&self) -> Option<AdvertisementKey> {
        let settings = self.settings.borrow();
        let session_key = settings.session_key.as_ref()?;
        Some(AdvertisementKey::derived_from(session_key.key_hexadecimal.as_bytes()))
    }

    async fn decide(&self, election: &mut Election, interface: &mut AddressOnInterface) {
        let settings = self.settings.borrow().clone();
        let is_eligible = settings.can_hold_the_address()
            && !self.claim.is_stepping_aside.load(Ordering::Relaxed)
            && dashboard_answers().await;
        election.set_eligible(is_eligible);
        let now = Instant::now();
        let claim = election.decide(now);
        interface.follow(claim, settings.address.as_ref()).await;
        self.claim.holds.store(interface.held.is_some(), Ordering::Relaxed);
        self.claim.is_held_by_a_peer.store(election.is_held_by_a_peer(now), Ordering::Relaxed);
    }

    async fn advertise(&self, socket: &UdpSocket, key: &AdvertisementKey, election: &Election) {
        let sealed = key.seal(&election.advertisement());
        for peer in &self.peers {
            if let Err(error) = socket.send_to(&sealed, (*peer, ADVERTISEMENT_PORT)).await {
                tracing::debug!(%error, %peer, "an advertisement could not be sent");
            }
        }
    }
}
