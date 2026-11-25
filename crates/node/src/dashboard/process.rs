use std::net::{IpAddr, Ipv4Addr};
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use amafu_api::server::{CertificateByLocalAddress, TlsIdentities, serve, tls_configuration};
use amafu_api::{HttpTransport, Transport};
use amafu_dashboard::{DashboardState, SessionKey, dashboard_router};
use amafu_hypervisor::control;
use amafu_hypervisor::sandbox::{self, Restrictions, SocketFamilies};
use anyhow::Context;
use serde::{Deserialize, Serialize};

pub const DASHBOARD_COMMAND: &str = "dashboard";
const SESSION_LIFETIME: Duration = Duration::from_secs(12 * 3600);
const READABLE: [&str; 3] = ["/proc", "/sys", "/dev/urandom"];

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TlsPair {
    pub certificate_pem: String,
    pub private_key_pem: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardHandOver {
    pub node_identity: TlsPair,
    pub cluster_identity: Option<(Ipv4Addr, TlsPair)>,
    pub certificate_authority_pem: String,
    pub session_key_hexadecimal: String,
    pub upstreams: Vec<String>,
}

impl DashboardHandOver {
    fn identities(&self) -> anyhow::Result<CertificateByLocalAddress> {
        let node = &self.node_identity;
        let identities = CertificateByLocalAddress::new(tls_configuration(
            node.certificate_pem.as_bytes(),
            node.private_key_pem.as_bytes(),
        )?);
        if let Some((address, cluster)) = &self.cluster_identity {
            let configuration =
                tls_configuration(cluster.certificate_pem.as_bytes(), cluster.private_key_pem.as_bytes())?;
            identities.present_at(IpAddr::V4(*address), configuration);
        }
        Ok(identities)
    }

    fn upstreams(&self) -> anyhow::Result<Vec<Arc<dyn Transport>>> {
        self.upstreams
            .iter()
            .map(|endpoint| {
                let transport =
                    HttpTransport::new(endpoint, Some(self.certificate_authority_pem.as_bytes()))?;
                Ok(Arc::new(transport) as Arc<dyn Transport>)
            })
            .collect()
    }
}

fn sandbox_restrictions() -> Restrictions {
    Restrictions { readable: READABLE.iter().map(PathBuf::from).collect(), writable: Vec::new() }
}

async fn serve_dashboard(hand_over: DashboardHandOver, listener: OwnedFd) -> anyhow::Result<()> {
    let listener = std::net::TcpListener::from(listener);
    listener.set_nonblocking(true)?;
    let listener = tokio::net::TcpListener::from_std(listener)?;
    let key = SessionKey::from_hexadecimal(&hand_over.session_key_hexadecimal)?;
    let state = DashboardState::new(key, hand_over.upstreams()?, SESSION_LIFETIME);
    let identities: Arc<dyn TlsIdentities> = Arc::new(hand_over.identities()?);
    tracing::info!(address = %listener.local_addr()?, "the dashboard is serving");
    Err(serve(listener, Some(identities), dashboard_router(state)).await.into())
}

pub fn run_dashboard_process() -> anyhow::Result<()> {
    let (hand_over, descriptors): (DashboardHandOver, Vec<OwnedFd>) =
        control::receive_message(std::io::stdin()).context("receiving the dashboard's hand-over")?;
    let listener = descriptors.into_iter().next().context("the hand-over carried no listening socket")?;
    sandbox::enter_with(&sandbox_restrictions(), SocketFamilies::UnixAndInternet)?;
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?
        .block_on(serve_dashboard(hand_over, listener))
}
