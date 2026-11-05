use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use amafu_api::{ClientIdentity, HttpTransport, NodeClient, Transport};
use amafu_controllers::node_lease::{LeaseKeeper, LeaseKeeperSettings};
use amafu_controllers::{ComputeController, ComputeServices, NodeController, NodeIdentity};
use amafu_domain::kinds::node::{CpuBaseline, NodeCapacity, NodeRole};
use amafu_domain::kinds::node_lease::LeaseTerms;
use amafu_domain::{NodeName, ObjectId, ObjectName};
use amafu_installer::certificates::CertificateAuthority;
use amafu_interfaces::{
    Clock, CoreConnection, Datapath, GuestAgents, Hypervisor, ImageStore, NodeLeaseAuthority, NodeLeaseState,
    Storage, Store,
};
use amafu_reconciler::{ReconcilerSettings, run_controller};
use amafu_simulated_world::fakes::{FakeBackends, FakeDatapath, FakeHypervisor, FakeImageStore, FakeStorage};
use amafu_state::{LeaseFencedStore, LocalStateCache, SubscribedStore};
use futures::future::BoxFuture;
use time::OffsetDateTime;
use tokio::task::JoinSet;

pub const TEST_BUNDLE: &str = "test-guest";
const BOOT_DURATION: Duration = Duration::from_secs(1);
const RESUBSCRIBE_DELAY: Duration = Duration::from_secs(1);
const LEASE_DURATION: Duration = Duration::from_secs(8);
const WATCHDOG_INTERVAL: Duration = Duration::from_secs(8);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const NODE_CAPACITY: NodeCapacity = NodeCapacity { cpu_count: 64, memory_mebibytes: 262_144 };

#[derive(Debug, Clone, Copy, Default)]
pub struct WallClock;

impl Clock for WallClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(duration))
    }
}

pub struct SimulatedFleet {
    tasks: JoinSet<()>,
    pub names: Vec<NodeName>,
}

struct NodeParts {
    name: NodeName,
    client: Arc<NodeClient>,
    store: SubscribedStore,
}

fn node_parts(
    index: usize,
    endpoints: &[String],
    authority: &CertificateAuthority,
) -> anyhow::Result<NodeParts> {
    let name = NodeName::new(format!("simulated-{index:04}"))?;
    let issued = authority.issue_node(&name, &[NodeRole::Compute])?;
    let identity =
        ClientIdentity { certificate_pem: issued.certificate_pem, private_key_pem: issued.private_key_pem };
    let transports = endpoints
        .iter()
        .map(|endpoint| {
            HttpTransport::with_identity(
                endpoint,
                Some(authority.certificate_pem.as_bytes()),
                Some(&identity),
            )
            .map(|transport| Arc::new(transport) as Arc<dyn Transport>)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let client = Arc::new(NodeClient::spreading(transports, &name));
    let store = SubscribedStore::new(
        name.clone(),
        Arc::clone(&client) as Arc<dyn CoreConnection>,
        LocalStateCache::in_memory(),
    );
    Ok(NodeParts { name, client, store })
}

fn spawn_controller<C: amafu_reconciler::Controller>(
    tasks: &mut JoinSet<()>,
    controller: C,
    store: Arc<dyn Store>,
    settings: ReconcilerSettings,
) {
    tasks.spawn(async move {
        loop {
            let failure = run_controller(&controller, store.as_ref(), &WallClock, settings).await;
            tracing::warn!(%failure, "a simulated node's controller stopped; restarting it");
            tokio::time::sleep(RESUBSCRIBE_DELAY).await;
        }
    });
}

impl SimulatedFleet {
    pub fn start(
        count: usize,
        endpoints: &[String],
        authority: &CertificateAuthority,
    ) -> anyhow::Result<Self> {
        let mut tasks = JoinSet::new();
        let mut names = Vec::new();
        let clock: Arc<dyn Clock> = Arc::new(WallClock);
        let backends = FakeBackends::new();
        for index in 0..count {
            let NodeParts { name, client, store } = node_parts(index, endpoints, authority)?;
            let follower = store.clone();
            tasks.spawn(async move {
                loop {
                    let ended = follower.follow().await;
                    tracing::debug!(%ended, "a simulated node's subscription ended");
                    tokio::time::sleep(RESUBSCRIBE_DELAY).await;
                }
            });
            let lease = NodeLeaseState::new();
            let authority: Arc<dyn NodeLeaseAuthority> = client;
            let (keeper_state, keeper_clock) = (lease.clone(), Arc::clone(&clock));
            tasks.spawn(async move {
                let keeper = LeaseKeeper {
                    authority: authority.as_ref(),
                    clock: keeper_clock.as_ref(),
                    state: &keeper_state,
                    settings: LeaseKeeperSettings::for_terms(LeaseTerms::new(
                        LEASE_DURATION,
                        WATCHDOG_INTERVAL,
                    )),
                };
                keeper.keep().await;
            });
            let fenced: Arc<dyn Store> = Arc::new(LeaseFencedStore::new(
                Arc::new(store),
                name.clone(),
                lease.clone(),
                Arc::clone(&clock),
            ));
            let identity = NodeIdentity {
                name: name.clone(),
                id: ObjectId::from_uuid(uuid::Uuid::new_v4()),
                roles: BTreeSet::from([NodeRole::Compute]),
                capacity: NODE_CAPACITY,
                cpu_baseline: CpuBaseline::X86_64Version3,
                zone: None,
                underlay_address: None,
                software_version: String::from(env!("CARGO_PKG_VERSION")),
                heartbeat_interval: HEARTBEAT_INTERVAL,
            };
            let heartbeat = ReconcilerSettings {
                periodic_resynchronization: HEARTBEAT_INTERVAL / 2,
                ..ReconcilerSettings::default()
            }
            .spread_by(name.spread());
            spawn_controller(
                &mut tasks,
                NodeController::new(identity, Arc::clone(&fenced), Arc::clone(&clock)),
                Arc::clone(&fenced),
                heartbeat,
            );
            let bundles = BTreeSet::from([ObjectName::new(TEST_BUNDLE)?]);
            let hypervisor = Arc::new(FakeHypervisor::new(Arc::clone(&clock), BOOT_DURATION, bundles));
            let services = ComputeServices {
                store: Arc::clone(&fenced),
                hypervisor: Arc::clone(&hypervisor) as Arc<dyn Hypervisor>,
                storage: Arc::new(FakeStorage::new(name.clone(), Arc::clone(&backends))) as Arc<dyn Storage>,
                images: Arc::new(FakeImageStore::new()) as Arc<dyn ImageStore>,
                datapath: Arc::new(FakeDatapath::new()) as Arc<dyn Datapath>,
                agents: hypervisor as Arc<dyn GuestAgents>,
                lease,
                clock: Arc::clone(&clock),
            };
            let settings = ReconcilerSettings::default().spread_by(name.spread());
            spawn_controller(&mut tasks, ComputeController::new(name.clone(), services), fenced, settings);
            names.push(name);
        }
        Ok(Self { tasks, names })
    }

    pub fn stop(&mut self) {
        self.tasks.abort_all();
    }
}
