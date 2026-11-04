use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::net::Ipv4Addr;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use amafu_api::authentication::TokenIdentity;
use amafu_api::client::Transport;
use amafu_api::diagnostics::SandboxCheck;
use amafu_api::{
    ApiClient, ApiState, NodeApiState, NodeAuthenticator, NodeClient, Principal, RouterTransport,
    TokenAuthenticator, node_router, router,
};
use amafu_checkers::invariants::check_all;
use amafu_controllers::node_lease::{LeaseKeeper, LeaseKeeperSettings, feed_while_leased};
use amafu_controllers::{
    CertificateRevocationController, ComputeController, ComputeServices, ExternalNetworkController,
    FloatingIpController, ImageController, LocalVolumeController, NetworkController, NodeController,
    NodeHealthController, NodeIdentity, NodeNetworkController, OperationSteps, OperationsController,
    SchedulerController, SchedulingSettings, SecurityGroupController, StorageBackendController,
    VolumeController,
};
use amafu_domain::access::Role;
use amafu_domain::kinds::external_network::ExternalNetworkSpecification;
use amafu_domain::kinds::node::{CpuBaseline, NodeCapacity, NodeRole};
use amafu_domain::kinds::node_lease::LeaseTerms;
use amafu_domain::kinds::storage_backend::StorageProviderSpecification;
use amafu_domain::{MacAddress, NodeName, ObjectId, ObjectName, ProjectName};
use amafu_interfaces::{
    AuditLog, Clock, CoreConnection, Datapath, Fence, GuestAgents, Hypervisor, Identifiers, ImageStore,
    Leases, LocalServices, NodeLeaseAuthority, NodeLeaseState, NodeNetworkState, Storage, Store,
    WorkloadState,
};
use amafu_network::dhcp::{self, DhcpMessage, Lease, MessageType};
use amafu_reconciler::{Controller, ReconcilerSettings, StepRegistry, run_controller, run_singleton};
use amafu_scenario::{
    CertificateAuthority, Cluster, ClusterObservation, Fault, FaultHandle, Level, NfsExport,
    ObservedAttachment, ObservedFencedWrite, ObservedFloatingAddress, ObservedInstance, ObservedPort,
    ObservedRemoteEndpoint, ScenarioDefinition, ScenarioError, ScenarioResult, TlsIdentity,
};
use amafu_state::{LeaseFencedStore, LocalNodeLeases, LocalStateCache, SubscribedStore};
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::clock::VirtualClock;
use crate::executor::{Executor, StepOutcome, TaskFuture, TaskId};
use crate::fakes::{
    FakeBackends, FakeDatapath, FakeHypervisor, FakeImageStore, FakeLeases, FakeLocalServices, FakeStorage,
    FakeWatchdog, MemoryAuditLog, MemoryStore, MigrationFabric, NodeLink, PartitionedStore,
    PartitionedTransport, SeededIdentifiers, simulated_ceph_rbd, simulated_nvme_tcp,
};

const PROJECTS: [&str; 2] = ["project-a", "project-b"];
const START_UNIX_SECONDS: i64 = 1_767_225_600;
const SIMULATED_NFS_SERVER: &str = "filer.lab";
const SIMULATED_NFS_EXPORT: &str = "/srv/amafu";
const SIMULATED_EXTERNAL_SUBNET: &str = "198.51.100.0/24";
const RECENT_EVENT_COUNT: usize = 200;
const ROLE_CRASH_PROBABILITY: f64 = 0.02;
const BOOT_DURATION: Duration = Duration::from_secs(1);
const TEST_BUNDLE: &str = "test-guest";
const CORE_MEMBER_COUNT: usize = 3;
const SUBSCRIPTION_REFRESH: Duration = Duration::from_secs(5);
const RESUBSCRIBE_DELAY: Duration = Duration::from_secs(1);
const LEASE_DURATION: Duration = Duration::from_secs(8);
const WATCHDOG_INTERVAL: Duration = Duration::from_secs(8);

fn lease_terms() -> LeaseTerms {
    LeaseTerms::new(LEASE_DURATION, WATCHDOG_INTERVAL)
}

struct NodeStores {
    store: Arc<dyn Store>,
    authority: Arc<dyn NodeLeaseAuthority>,
    follower: Option<RoleTask>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimulationSettings {
    pub seed: u64,
    pub crashes_roles: bool,
    pub maximum_steps: u64,
}

impl SimulationSettings {
    #[must_use]
    pub fn with_seed(seed: u64) -> Self {
        Self { seed, crashes_roles: false, maximum_steps: 2_000_000 }
    }

    #[must_use]
    pub fn crashing_roles(mut self) -> Self {
        self.crashes_roles = true;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReport {
    pub seed: u64,
    pub steps: u64,
    pub virtual_duration: Duration,
    pub store_writes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimulationFailure {
    pub seed: u64,
    pub step: u64,
    pub reason: String,
    pub recent_events: Vec<String>,
}

impl fmt::Display for SimulationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "seed {} failed at step {}: {}", self.seed, self.step, self.reason)?;
        writeln!(formatter, "replay with: cargo rig replay {}", self.seed)?;
        for event in &self.recent_events {
            writeln!(formatter, "  {event}")?;
        }
        Ok(())
    }
}

impl std::error::Error for SimulationFailure {}

struct RoleTask {
    label: String,
    start: Rc<dyn Fn() -> TaskFuture>,
    task: Cell<TaskId>,
}

struct NodeServices {
    lease: NodeLeaseState,
    watchdog: Arc<FakeWatchdog>,
    hypervisor: Arc<FakeHypervisor>,
    storage: Arc<FakeStorage>,
    images: Arc<FakeImageStore>,
    datapath: Arc<FakeDatapath>,
    local_services: Arc<FakeLocalServices>,
}

impl NodeServices {
    fn compute(&self, store: &Arc<dyn Store>, clock: &Arc<dyn Clock>) -> ComputeServices {
        ComputeServices {
            lease: self.lease.clone(),
            clock: Arc::clone(clock),
            store: Arc::clone(store),
            hypervisor: Arc::clone(&self.hypervisor) as Arc<dyn Hypervisor>,
            storage: Arc::clone(&self.storage) as Arc<dyn Storage>,
            images: Arc::clone(&self.images) as Arc<dyn ImageStore>,
            datapath: Arc::clone(&self.datapath) as Arc<dyn Datapath>,
            agents: Arc::clone(&self.hypervisor) as Arc<dyn GuestAgents>,
        }
    }

    fn power_off(&self) {
        self.lease.drop_lease();
        self.watchdog.disarm();
        self.hypervisor.power_off();
        self.storage.power_off();
        self.datapath.power_off();
        self.local_services.power_off();
    }
}

struct SimulatedNode {
    services: NodeServices,
    roles: Vec<RoleTask>,
    link: Arc<NodeLink>,
    is_core: bool,
    is_powered: Cell<bool>,
}

struct WorldShared {
    seed: u64,
    crashes_roles: bool,
    random: RefCell<ChaCha8Rng>,
    executor: Executor,
    clock: VirtualClock,
    store: Arc<MemoryStore>,
    leases: Arc<FakeLeases>,
    backends: Arc<FakeBackends>,
    fabric: Arc<MigrationFabric>,
    audit: Arc<MemoryAuditLog>,
    router: axum::Router,
    tokens: BTreeMap<(Role, ProjectName), String>,
    expired_token: String,
    authority: CertificateAuthority,
    active_faults: RefCell<BTreeMap<FaultHandle, Fault>>,
    nodes: RefCell<BTreeMap<NodeName, SimulatedNode>>,
    failures: Rc<RefCell<Vec<String>>>,
    recent_events: RefCell<VecDeque<String>>,
    next_fault: Cell<u64>,
}

#[derive(Clone)]
pub struct SimulatedWorld {
    shared: Rc<WorldShared>,
}

fn start_time() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(START_UNIX_SECONDS).unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

fn token_from(identifiers: &SeededIdentifiers) -> String {
    format!("amafu-{}{}", identifiers.generate().simple(), identifiers.generate().simple())
}

fn valid_name<T: std::str::FromStr>(value: &str) -> T {
    value.parse().unwrap_or_else(|_| unreachable!())
}

fn controller_role<C: Controller>(
    label: String,
    make: impl Fn() -> C + 'static,
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    settings: ReconcilerSettings,
    failures: Rc<RefCell<Vec<String>>>,
) -> RoleTask {
    let task_label = label.clone();
    let start: Rc<dyn Fn() -> TaskFuture> = Rc::new(move || {
        let controller = make();
        let store = Arc::clone(&store);
        let clock = Arc::clone(&clock);
        let failures = Rc::clone(&failures);
        let label = task_label.clone();
        Box::pin(async move {
            let failure = run_controller(&controller, store.as_ref(), clock.as_ref(), settings).await;
            failures.borrow_mut().push(format!("{label}: {failure}"));
        })
    });
    RoleTask { label, start, task: Cell::new(TaskId::default()) }
}

struct SingletonServices {
    leases: Arc<dyn Leases>,
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    failures: Rc<RefCell<Vec<String>>>,
}

fn singleton_role<C: Controller>(
    label: String,
    make: impl Fn(Fence) -> C + Clone + 'static,
    services: &SingletonServices,
    settings: ReconcilerSettings,
) -> RoleTask {
    let task_label = label.clone();
    let (leases, store, clock, failures) = (
        Arc::clone(&services.leases),
        Arc::clone(&services.store),
        Arc::clone(&services.clock),
        Rc::clone(&services.failures),
    );
    let start: Rc<dyn Fn() -> TaskFuture> = Rc::new(move || {
        let (leases, store, clock, failures) =
            (Arc::clone(&leases), Arc::clone(&store), Arc::clone(&clock), Rc::clone(&failures));
        let (make, label) = (make.clone(), task_label.clone());
        Box::pin(async move {
            let failure =
                run_singleton(leases.as_ref(), make, store.as_ref(), clock.as_ref(), settings).await;
            failures.borrow_mut().push(format!("{label}: {failure}"));
        })
    });
    RoleTask { label, start, task: Cell::new(TaskId::default()) }
}

impl SimulatedWorld {
    #[must_use]
    pub fn new(settings: &SimulationSettings, node_count: usize) -> Self {
        let clock = VirtualClock::starting_at(start_time());
        let identifiers = Arc::new(SeededIdentifiers::new(settings.seed));
        let credentials = SeededIdentifiers::new(settings.seed.wrapping_add(1));
        let mut tokens = BTreeMap::new();
        let mut identities = Vec::new();
        for project in PROJECTS.map(valid_name::<ProjectName>) {
            for role in Role::ALL {
                let token = token_from(&credentials);
                let principal =
                    Principal { name: format!("{role}-of-{project}"), role, project: project.clone() };
                identities.push(TokenIdentity::for_token(principal, &token, None));
                tokens.insert((role, project.clone()), token);
            }
        }
        let expired_token = token_from(&credentials);
        let expired = Principal {
            name: String::from("former-admin"),
            role: Role::Admin,
            project: valid_name(PROJECTS[0]),
        };
        identities.push(TokenIdentity::for_token(
            expired,
            &expired_token,
            Some(start_time() - Duration::from_secs(1)),
        ));

        let store = Arc::new(MemoryStore::new());
        let audit = Arc::new(MemoryAuditLog::new());
        let authority =
            CertificateAuthority::generate("Amafu simulated cluster").unwrap_or_else(|_| unreachable!());
        let node_authenticator = NodeAuthenticator::new(
            authority.certificate_pem.as_bytes(),
            Arc::clone(&store) as Arc<dyn Store>,
        )
        .unwrap_or_else(|_| unreachable!());
        let router = router(ApiState {
            store: Arc::clone(&store) as Arc<dyn Store>,
            authenticator: Arc::new(TokenAuthenticator::new(identities)),
            audit: Arc::clone(&audit) as Arc<dyn AuditLog>,
            clock: Arc::new(clock.clone()),
            identifiers: Arc::clone(&identifiers) as Arc<dyn Identifiers>,
        })
        .merge(node_router(NodeApiState {
            store: Arc::clone(&store) as Arc<dyn Store>,
            authenticator: Arc::new(node_authenticator),
            audit: Arc::clone(&audit) as Arc<dyn AuditLog>,
            clock: Arc::new(clock.clone()),
            identifiers: Arc::clone(&identifiers) as Arc<dyn Identifiers>,
            refresh_interval: SUBSCRIPTION_REFRESH,
        }));
        let world = Self {
            shared: Rc::new(WorldShared {
                seed: settings.seed,
                crashes_roles: settings.crashes_roles,
                random: RefCell::new(ChaCha8Rng::seed_from_u64(settings.seed)),
                executor: Executor::new(clock.clone()),
                clock,
                store,
                leases: FakeLeases::new(),
                backends: FakeBackends::new(),
                fabric: MigrationFabric::new(),
                audit,
                router,
                tokens,
                expired_token,
                authority,
                active_faults: RefCell::new(BTreeMap::new()),
                nodes: RefCell::new(BTreeMap::new()),
                failures: Rc::new(RefCell::new(Vec::new())),
                recent_events: RefCell::new(VecDeque::new()),
                next_fault: Cell::new(0),
            }),
        };
        for index in 0..node_count {
            world.add_node(u8::try_from(index).unwrap_or(u8::MAX), index < CORE_MEMBER_COUNT, &identifiers);
        }
        world
    }

    fn role<C: Controller>(
        &self,
        label: String,
        store: &Arc<dyn Store>,
        settings: ReconcilerSettings,
        make: impl Fn() -> C + 'static,
    ) -> RoleTask {
        let shared = &self.shared;
        controller_role(
            label,
            make,
            Arc::clone(store),
            Arc::new(shared.clock.clone()),
            settings,
            Rc::clone(&shared.failures),
        )
    }

    fn subscription_role(&self, label: String, store: SubscribedStore) -> RoleTask {
        let clock = self.shared.clock.clone();
        let start: Rc<dyn Fn() -> TaskFuture> = Rc::new(move || {
            let (store, clock) = (store.clone(), clock.clone());
            Box::pin(async move {
                loop {
                    let _ended = store.follow().await;
                    clock.sleep(RESUBSCRIBE_DELAY).await;
                }
            })
        });
        RoleTask { label, start, task: Cell::new(TaskId::default()) }
    }

    fn node_store(
        &self,
        name: &NodeName,
        is_core: bool,
        link: &Arc<NodeLink>,
        identifiers: &Arc<SeededIdentifiers>,
    ) -> NodeStores {
        let shared = &self.shared;
        if is_core {
            let store: Arc<dyn Store> = Arc::new(PartitionedStore::new(
                Arc::clone(&shared.store) as Arc<dyn Store>,
                Arc::clone(link),
            ));
            let authority = Arc::new(LocalNodeLeases::new(
                Arc::clone(&store),
                name.clone(),
                Arc::new(shared.clock.clone()),
                Arc::clone(identifiers) as Arc<dyn Identifiers>,
            ));
            return NodeStores { store, authority, follower: None };
        }
        let identity =
            shared.authority.issue_node(name, &[NodeRole::Compute]).unwrap_or_else(|_| unreachable!());
        let transport: Arc<dyn Transport> = Arc::new(PartitionedTransport::new(
            Arc::new(RouterTransport::presenting(shared.router.clone(), identity.certificate_der)),
            Arc::clone(link),
        ));
        let client = Arc::new(NodeClient::new(vec![transport]));
        let store = SubscribedStore::new(
            name.clone(),
            Arc::clone(&client) as Arc<dyn CoreConnection>,
            LocalStateCache::in_memory(),
        );
        let follower = self.subscription_role(format!("{name}/subscription"), store.clone());
        NodeStores { store: Arc::new(store), authority: client, follower: Some(follower) }
    }

    fn lease_roles(
        &self,
        name: &NodeName,
        authority: Arc<dyn NodeLeaseAuthority>,
        services: &NodeServices,
    ) -> [RoleTask; 2] {
        let clock: Arc<dyn Clock> = Arc::new(self.shared.clock.clone());
        let (keeper_clock, keeper_state) = (Arc::clone(&clock), services.lease.clone());
        let keep: Rc<dyn Fn() -> TaskFuture> = Rc::new(move || {
            let (authority, clock, state) =
                (Arc::clone(&authority), Arc::clone(&keeper_clock), keeper_state.clone());
            Box::pin(async move {
                let keeper = LeaseKeeper {
                    authority: authority.as_ref(),
                    clock: clock.as_ref(),
                    state: &state,
                    settings: LeaseKeeperSettings::for_terms(lease_terms()),
                };
                keeper.keep().await;
            })
        });
        let (watchdog, feeder_state) = (Arc::clone(&services.watchdog), services.lease.clone());
        let feed: Rc<dyn Fn() -> TaskFuture> = Rc::new(move || {
            let (watchdog, state, clock) = (Arc::clone(&watchdog), feeder_state.clone(), Arc::clone(&clock));
            Box::pin(async move {
                feed_while_leased(watchdog.as_ref(), &state, clock.as_ref(), WATCHDOG_INTERVAL).await;
            })
        });
        [
            RoleTask { label: format!("{name}/lease"), start: keep, task: Cell::new(TaskId::default()) },
            RoleTask { label: format!("{name}/watchdog"), start: feed, task: Cell::new(TaskId::default()) },
        ]
    }

    fn node_services(
        &self,
        name: &NodeName,
        clock: &Arc<dyn Clock>,
        underlay_address: Ipv4Addr,
        link: &Arc<NodeLink>,
    ) -> NodeServices {
        let bundles = BTreeSet::from([valid_name::<ObjectName>(TEST_BUNDLE)]);
        NodeServices {
            lease: NodeLeaseState::new(),
            watchdog: Arc::new(FakeWatchdog::new(Arc::clone(clock), WATCHDOG_INTERVAL)),
            hypervisor: Arc::new(FakeHypervisor::on_fabric(
                Arc::clone(clock),
                BOOT_DURATION,
                bundles,
                Arc::clone(&self.shared.fabric),
                underlay_address,
                Arc::clone(link),
            )),
            storage: Arc::new(FakeStorage::new(name.clone(), Arc::clone(&self.shared.backends))),
            images: Arc::new(FakeImageStore::new()),
            datapath: Arc::new(FakeDatapath::new()),
            local_services: Arc::new(FakeLocalServices::new()),
        }
    }

    fn add_node(&self, index: u8, is_core: bool, identifiers: &Arc<SeededIdentifiers>) {
        let name: &NodeName = &valid_name(&format!("node-{index}"));
        let underlay_address = Ipv4Addr::new(192, 0, 2, index.saturating_add(10));
        let shared = &self.shared;
        let clock: Arc<dyn Clock> = Arc::new(shared.clock.clone());
        let link = NodeLink::new();
        let NodeStores { store: shared_store, authority, follower } =
            self.node_store(name, is_core, &link, identifiers);
        let services = self.node_services(name, &clock, underlay_address, &link);
        let store: Arc<dyn Store> = Arc::new(LeaseFencedStore::new(
            Arc::clone(&shared_store),
            name.clone(),
            services.lease.clone(),
            Arc::clone(&clock),
        ));
        let mut roles = BTreeSet::from([NodeRole::Compute]);
        if is_core {
            roles.insert(NodeRole::Core);
        }
        let identity = NodeIdentity {
            name: name.clone(),
            id: ObjectId::from_uuid(identifiers.generate()),
            roles,
            capacity: NodeCapacity { cpu_count: 16, memory_mebibytes: 65_536 },
            cpu_baseline: CpuBaseline::X86_64Version3,
            zone: None,
            underlay_address: Some(underlay_address),
            software_version: String::from(env!("CARGO_PKG_VERSION")),
            heartbeat_interval: Duration::from_secs(10),
        };
        let fast = ReconcilerSettings {
            initial_backoff: Duration::from_millis(100),
            maximum_backoff: Duration::from_secs(5),
            periodic_resynchronization: Duration::from_secs(2),
        };
        let heartbeat = ReconcilerSettings { periodic_resynchronization: Duration::from_secs(5), ..fast };
        let compute = services.compute(&store, &clock);
        let (network_store, datapath, local_services) =
            (Arc::clone(&store), Arc::clone(&services.datapath), Arc::clone(&services.local_services));
        let (node_store, node_clock, node_name) = (Arc::clone(&store), Arc::clone(&clock), name.clone());
        let (image_store, image_cache, image_node) =
            (Arc::clone(&store), Arc::clone(&services.images) as Arc<dyn ImageStore>, name.clone());
        let (volume_store, volume_storage, volume_images, volume_node) = (
            Arc::clone(&store),
            Arc::clone(&services.storage) as Arc<dyn Storage>,
            Arc::clone(&services.images) as Arc<dyn ImageStore>,
            name.clone(),
        );
        let mut tasks: Vec<RoleTask> = follower.into_iter().collect();
        tasks.extend(self.lease_roles(name, authority, &services));
        tasks.extend([
            self.role(format!("{name}/node"), &store, heartbeat, move || {
                NodeController::new(identity.clone(), Arc::clone(&node_store), Arc::clone(&node_clock))
            }),
            self.role(format!("{name}/compute"), &store, fast, move || {
                ComputeController::new(node_name.clone(), compute.clone())
            }),
            self.role(format!("{name}/local-volume"), &store, fast, move || {
                LocalVolumeController::new(
                    volume_node.clone(),
                    Arc::clone(&volume_store),
                    Arc::clone(&volume_storage),
                    Arc::clone(&volume_images),
                )
            }),
            self.role(format!("{name}/image"), &store, fast, move || {
                ImageController::new(
                    image_node.clone(),
                    Arc::clone(&image_store),
                    Arc::clone(&image_cache),
                    is_core,
                )
            }),
        ]);
        let node_name = name.clone();
        tasks.push(self.role(format!("{name}/node-network"), &store, fast, move || {
            NodeNetworkController::new(
                node_name.clone(),
                Some(underlay_address),
                Arc::clone(&network_store),
                Arc::clone(&datapath) as Arc<dyn Datapath>,
                Arc::clone(&local_services) as Arc<dyn LocalServices>,
            )
        }));
        if is_core {
            tasks.extend(self.core_roles(name, &shared_store, &services, fast));
            tasks.extend(self.recovery_roles(name, &shared_store, &services, identifiers, fast));
        }
        for role in &tasks {
            role.task.set(shared.executor.spawn(role.label.clone(), (role.start)()));
        }
        shared.nodes.borrow_mut().insert(
            name.clone(),
            SimulatedNode { services, roles: tasks, link, is_core, is_powered: Cell::new(true) },
        );
    }

    fn recovery_roles(
        &self,
        name: &NodeName,
        store: &Arc<dyn Store>,
        services: &NodeServices,
        identifiers: &Arc<SeededIdentifiers>,
        settings: ReconcilerSettings,
    ) -> Vec<RoleTask> {
        let shared = &self.shared;
        let singletons = SingletonServices {
            leases: Arc::new(shared.leases.for_node(name.clone())),
            store: Arc::clone(store),
            clock: Arc::new(shared.clock.clone()),
            failures: Rc::clone(&shared.failures),
        };
        let storage: Arc<dyn Storage> = Arc::clone(&services.storage) as Arc<dyn Storage>;
        let (health_store, health_clock, health_identifiers) = (
            Arc::clone(store),
            Arc::new(shared.clock.clone()) as Arc<dyn Clock>,
            Arc::clone(identifiers) as Arc<dyn Identifiers>,
        );
        let (operations_store, operations_clock, operations_steps) = (
            Arc::clone(store),
            Arc::new(shared.clock.clone()) as Arc<dyn Clock>,
            Arc::new(OperationSteps::new(Arc::clone(&storage))) as Arc<dyn StepRegistry>,
        );
        vec![
            singleton_role(
                format!("{name}/node-health"),
                move |_| {
                    NodeHealthController::new(
                        Arc::clone(&health_store),
                        Arc::clone(&health_clock),
                        Arc::clone(&health_identifiers),
                    )
                },
                &singletons,
                settings,
            ),
            singleton_role(
                format!("{name}/operations"),
                move |fence| {
                    OperationsController::new(
                        Arc::clone(&operations_store),
                        Arc::clone(&operations_clock),
                        Arc::clone(&operations_steps),
                        fence,
                    )
                },
                &singletons,
                settings,
            ),
        ]
    }

    fn core_roles(
        &self,
        name: &NodeName,
        store: &Arc<dyn Store>,
        services: &NodeServices,
        settings: ReconcilerSettings,
    ) -> Vec<RoleTask> {
        let shared = &self.shared;
        let singletons = SingletonServices {
            leases: Arc::new(shared.leases.for_node(name.clone())),
            store: Arc::clone(store),
            clock: Arc::new(shared.clock.clone()),
            failures: Rc::clone(&shared.failures),
        };
        let storage: Arc<dyn Storage> = Arc::clone(&services.storage) as Arc<dyn Storage>;
        let images: Arc<dyn ImageStore> = Arc::clone(&services.images) as Arc<dyn ImageStore>;
        let (scheduler_store, network_store, group_store) =
            (Arc::clone(store), Arc::clone(store), Arc::clone(store));
        let (volume_store, volume_storage, volume_images) =
            (Arc::clone(store), Arc::clone(&storage), Arc::clone(&images));
        let (backend_store, backend_storage) = (Arc::clone(store), Arc::clone(&storage));
        let floating_store = Arc::clone(store);
        let (external_store, revocation_store) = (Arc::clone(store), Arc::clone(store));
        vec![
            singleton_role(
                format!("{name}/external-network"),
                move |_| ExternalNetworkController::new(Arc::clone(&external_store)),
                &singletons,
                settings,
            ),
            singleton_role(
                format!("{name}/certificate-revocation"),
                move |_| CertificateRevocationController::new(Arc::clone(&revocation_store)),
                &singletons,
                settings,
            ),
            singleton_role(
                format!("{name}/floating-ip"),
                move |_| FloatingIpController::new(Arc::clone(&floating_store)),
                &singletons,
                settings,
            ),
            singleton_role(
                format!("{name}/scheduler"),
                move |fence| {
                    SchedulerController::new(
                        Arc::clone(&scheduler_store),
                        fence,
                        SchedulingSettings::default(),
                    )
                },
                &singletons,
                settings,
            ),
            singleton_role(
                format!("{name}/network"),
                move |_| NetworkController::new(Arc::clone(&network_store)),
                &singletons,
                settings,
            ),
            singleton_role(
                format!("{name}/security-group"),
                move |_| SecurityGroupController::new(Arc::clone(&group_store)),
                &singletons,
                settings,
            ),
            singleton_role(
                format!("{name}/volume"),
                move |_| {
                    VolumeController::new(
                        Arc::clone(&volume_store),
                        Arc::clone(&volume_storage),
                        Arc::clone(&volume_images),
                    )
                },
                &singletons,
                settings,
            ),
            singleton_role(
                format!("{name}/storage-backend"),
                move |_| {
                    StorageBackendController::new(Arc::clone(&backend_store), Arc::clone(&backend_storage))
                },
                &singletons,
                settings,
            ),
        ]
    }

    fn record(&self, event: String) {
        let mut events = self.shared.recent_events.borrow_mut();
        if events.len() == RECENT_EVENT_COUNT {
            events.pop_front();
        }
        events.push_back(event);
    }

    fn restart_role(&self, role: &RoleTask) {
        self.shared.executor.kill(role.task.get());
        role.task.set(self.shared.executor.spawn(role.label.clone(), (role.start)()));
    }

    fn power_cycle(&self, node: &NodeName) -> Result<(), ScenarioError> {
        let nodes = self.shared.nodes.borrow();
        let simulated =
            nodes.get(node).ok_or_else(|| ScenarioError::Expectation(format!("no node named {node}")))?;
        simulated.services.power_off();
        for role in &simulated.roles {
            self.restart_role(role);
        }
        drop(nodes);
        self.record(format!("power cycled {node}"));
        Ok(())
    }

    fn set_powered(&self, node: &NodeName, is_powered: bool) -> Result<(), ScenarioError> {
        let nodes = self.shared.nodes.borrow();
        let simulated =
            nodes.get(node).ok_or_else(|| ScenarioError::Expectation(format!("no node named {node}")))?;
        if is_powered {
            simulated.link.restore();
            self.shared.leases.reconnect(node);
            for role in &simulated.roles {
                self.restart_role(role);
            }
        } else {
            simulated.services.power_off();
            simulated.link.cut();
            if simulated.is_core {
                self.shared.leases.isolate(node);
            }
            for role in &simulated.roles {
                self.shared.executor.kill(role.task.get());
            }
        }
        simulated.is_powered.set(is_powered);
        drop(nodes);
        self.record(format!("{} {node}", if is_powered { "powered on" } else { "powered off" }));
        Ok(())
    }

    fn set_partitioned(&self, node: &NodeName, is_partitioned: bool) -> Result<(), ScenarioError> {
        let nodes = self.shared.nodes.borrow();
        let simulated =
            nodes.get(node).ok_or_else(|| ScenarioError::Expectation(format!("no node named {node}")))?;
        if is_partitioned {
            simulated.link.cut();
            if simulated.is_core {
                self.shared.leases.isolate(node);
            }
        } else {
            simulated.link.restore();
            self.shared.leases.reconnect(node);
        }
        drop(nodes);
        self.record(format!("{} {node}", if is_partitioned { "partitioned" } else { "reconnected" }));
        Ok(())
    }

    fn maybe_crash_a_role(&self) {
        if !self.shared.crashes_roles || !self.shared.random.borrow_mut().random_bool(ROLE_CRASH_PROBABILITY)
        {
            return;
        }
        let nodes = self.shared.nodes.borrow();
        let roles: Vec<&RoleTask> =
            nodes.values().filter(|node| node.is_powered.get()).flat_map(|node| node.roles.iter()).collect();
        if roles.is_empty() {
            return;
        }
        let role = roles[self.shared.random.borrow_mut().random_range(0..roles.len())];
        self.restart_role(role);
        let label = role.label.clone();
        drop(nodes);
        self.record(format!("crashed and restarted {label}"));
    }

    #[must_use]
    pub fn ground_truth(&self) -> ClusterObservation {
        let fenced_writes = self
            .shared
            .store
            .fenced_writes()
            .into_iter()
            .map(|write| ObservedFencedWrite {
                object: write.reference.to_string(),
                fence: write.fence.name,
                token: write.fence.token,
                is_accepted: write.is_accepted,
            })
            .collect();
        let mut observation = ClusterObservation {
            stored_objects: self.shared.store.all_objects(),
            fenced_writes,
            ..ClusterObservation::default()
        };
        for (name, node) in self.shared.nodes.borrow().iter() {
            let services = &node.services;
            let offered = services.local_services.state();
            for (running, instance) in services.hypervisor.instances() {
                let offered_addresses = instance
                    .launch
                    .network_interfaces
                    .iter()
                    .filter_map(|interface| address_offered_to(&offered, interface.mac))
                    .collect();
                let workload = match (&instance.workload, instance.exit_code) {
                    (_, Some(code)) => Some(WorkloadState::Exited { code }),
                    (Some(_), None) => Some(WorkloadState::Running),
                    (None, None) if instance.launch.has_guest_agent => Some(WorkloadState::NotStarted),
                    (None, None) => None,
                };
                observation.instances.push(ObservedInstance {
                    node: name.clone(),
                    vm_id: running.vm_id,
                    state: running.state,
                    workload,
                    disks: instance.launch.disks.iter().map(|disk| disk.path.clone()).collect(),
                    offered_addresses,
                    console: String::new(),
                });
            }
            for (target, path) in services.storage.attachments() {
                observation.attachments.push(ObservedAttachment {
                    node: name.clone(),
                    volume: format!("{target:?}"),
                    path,
                });
            }
            let programmed = services.datapath.state();
            for port in programmed.ports {
                observation.ports.push(ObservedPort {
                    node: name.clone(),
                    vm_id: port.key.vm_id,
                    segment: port.segment.to_string(),
                    address: port.address,
                });
            }
            for remote in programmed.remote_endpoints {
                observation.remote_endpoints.push(ObservedRemoteEndpoint {
                    node: name.clone(),
                    segment: remote.segment.to_string(),
                    address: remote.address,
                    node_address: remote.node_address,
                });
            }
            for floating in programmed.floating_addresses {
                observation.floating_addresses.push(ObservedFloatingAddress {
                    node: name.clone(),
                    address: floating.address,
                    private_address: floating.private_address,
                });
            }
        }
        observation
    }

    pub fn exit_workload(&self, vm_id: &ObjectId, code: i32) {
        for node in self.shared.nodes.borrow().values() {
            node.services.hypervisor.exit_workload(vm_id, code);
        }
    }

    #[must_use]
    pub fn step(&self) -> StepOutcome {
        let outcome = self.shared.executor.step(&mut self.shared.random.borrow_mut());
        match &outcome {
            StepOutcome::Polled { label } => {
                self.record(format!("{} polled {label}", self.shared.clock.now()));
            }
            StepOutcome::AdvancedClock => {
                self.record(format!("clock advanced to {}", self.shared.clock.now()));
            }
            StepOutcome::Idle => self.record(String::from("idle")),
        }
        self.maybe_crash_a_role();
        self.reset_nodes_whose_watchdog_fired();
        outcome
    }

    fn reset_nodes_whose_watchdog_fired(&self) {
        let fired: Vec<NodeName> = self
            .shared
            .nodes
            .borrow()
            .iter()
            .filter(|(_, node)| node.services.watchdog.has_fired())
            .map(|(name, _)| name.clone())
            .collect();
        for node in fired {
            if self.power_cycle(&node).is_ok() {
                self.record(format!("the watchdog reset {node}"));
            }
        }
    }

    fn failure(&self, step: u64, reason: String) -> SimulationFailure {
        SimulationFailure {
            seed: self.shared.seed,
            step,
            reason,
            recent_events: self.shared.recent_events.borrow().iter().cloned().collect(),
        }
    }

    fn drive(
        &self,
        outcome: &Rc<RefCell<Option<ScenarioResult>>>,
        maximum_steps: u64,
    ) -> Result<RunReport, SimulationFailure> {
        for step in 1..=maximum_steps {
            if let Some(result) = outcome.borrow_mut().take() {
                return match result {
                    Ok(()) => Ok(RunReport {
                        seed: self.shared.seed,
                        steps: step,
                        virtual_duration: Duration::try_from(self.shared.clock.now() - start_time())
                            .unwrap_or_default(),
                        store_writes: self.shared.store.write_count(),
                    }),
                    Err(error) => Err(self.failure(step, error.to_string())),
                };
            }
            if self.step() == StepOutcome::Idle {
                return Err(self.failure(step, String::from("every task is blocked and no timer is pending")));
            }
            if let Err(violation) = check_all(&self.ground_truth()) {
                return Err(self.failure(step, format!("invariant violated: {violation}")));
            }
            if let Some(failure) = self.shared.failures.borrow().first() {
                return Err(self.failure(step, format!("a role failed: {failure}")));
            }
        }
        Err(self.failure(maximum_steps, String::from("the step budget was exhausted")))
    }

    fn shutdown(&self) {
        self.shared.executor.kill_all();
    }
}

fn address_offered_to(state: &NodeNetworkState, mac: MacAddress) -> Option<Ipv4Addr> {
    let discover = DhcpMessage {
        is_reply: false,
        transaction: 1,
        flags: 0,
        client_address: Ipv4Addr::UNSPECIFIED,
        your_address: Ipv4Addr::UNSPECIFIED,
        relay_address: Ipv4Addr::UNSPECIFIED,
        hardware_address: mac,
        message_type: MessageType::Discover,
        requested_address: None,
        server_identifier: None,
        lease: Lease::default(),
        options: Vec::new(),
    };
    let request = DhcpMessage::parse(&discover.encode()).ok()?;
    state
        .segments
        .iter()
        .find_map(|segment| dhcp::answer(&request, &segment.key, state))
        .map(|offer| offer.your_address)
}

pub fn run_scenario(
    definition: &ScenarioDefinition,
    settings: SimulationSettings,
) -> Result<RunReport, SimulationFailure> {
    let world = SimulatedWorld::new(&settings, definition.node_count);
    let outcome = Rc::new(RefCell::new(None));
    let scenario = definition.run;
    let cluster = world.clone();
    let slot = Rc::clone(&outcome);
    world.shared.executor.spawn("scenario", async move {
        let result = scenario(&cluster).await;
        *slot.borrow_mut() = Some(result);
    });
    let result = world.drive(&outcome, settings.maximum_steps);
    world.shutdown();
    result
}

#[async_trait::async_trait(?Send)]
impl Cluster for SimulatedWorld {
    async fn client_in_project(&self, role: Role, project: &ProjectName) -> ApiClient {
        let token = self.shared.tokens.get(&(role, project.clone())).cloned();
        ApiClient::new(Arc::new(RouterTransport::new(self.shared.router.clone())), token)
            .in_project(project.clone())
    }

    async fn core_member_clients(&self, role: Role, project: &ProjectName) -> Vec<ApiClient> {
        let core_members = self.shared.nodes.borrow().values().filter(|node| node.is_core).count();
        let mut clients = Vec::new();
        for _ in 0..core_members.max(1) {
            clients.push(self.client_in_project(role, project).await);
        }
        clients
    }

    async fn anonymous_client(&self) -> ApiClient {
        ApiClient::new(Arc::new(RouterTransport::new(self.shared.router.clone())), None)
    }

    async fn expired_client(&self) -> ApiClient {
        ApiClient::new(
            Arc::new(RouterTransport::new(self.shared.router.clone())),
            Some(self.shared.expired_token.clone()),
        )
    }

    async fn nodes(&self) -> Vec<NodeName> {
        self.shared.nodes.borrow().keys().cloned().collect()
    }

    async fn inject(&self, fault: Fault) -> Result<FaultHandle, ScenarioError> {
        match &fault {
            Fault::PowerCycle { node } => self.power_cycle(node)?,
            Fault::PowerOff { node } => self.set_powered(node, false)?,
            Fault::Partition { node } => self.set_partitioned(node, true)?,
        }
        let handle = FaultHandle(self.shared.next_fault.get());
        self.shared.next_fault.set(handle.0 + 1);
        self.shared.active_faults.borrow_mut().insert(handle, fault);
        Ok(handle)
    }

    async fn heal(&self, handle: FaultHandle) -> Result<(), ScenarioError> {
        let fault = self.shared.active_faults.borrow_mut().remove(&handle);
        match fault {
            Some(Fault::Partition { node }) => self.set_partitioned(&node, false)?,
            Some(Fault::PowerOff { node }) => self.set_powered(&node, true)?,
            Some(Fault::PowerCycle { .. }) | None => {}
        }
        Ok(())
    }

    async fn observe(&self) -> Result<ClusterObservation, ScenarioError> {
        Ok(self.ground_truth())
    }

    async fn audit_contains(&self, request_id: Uuid) -> Result<bool, ScenarioError> {
        let found = self
            .shared
            .audit
            .find(request_id)
            .await
            .map_err(|error| ScenarioError::Lab(error.to_string()))?;
        Ok(found.is_some())
    }

    async fn sandbox_checks(&self) -> Result<Option<Vec<SandboxCheck>>, ScenarioError> {
        Ok(None)
    }

    fn certificate_authority(&self) -> &CertificateAuthority {
        &self.shared.authority
    }

    fn external_network(&self) -> ExternalNetworkSpecification {
        ExternalNetworkSpecification {
            subnet: valid_name(SIMULATED_EXTERNAL_SUBNET),
            first_address: Ipv4Addr::new(198, 51, 100, 10),
            last_address: Ipv4Addr::new(198, 51, 100, 29),
        }
    }

    fn block_backends(&self) -> Vec<(String, StorageProviderSpecification)> {
        vec![
            (String::from("nvme-tcp"), simulated_nvme_tcp()),
            (String::from("ceph-rbd"), simulated_ceph_rbd()),
        ]
    }

    async fn probe_from_outside(
        &self,
        _address: Ipv4Addr,
        _port: u16,
    ) -> Result<Option<String>, ScenarioError> {
        Ok(None)
    }

    async fn node_connection(
        &self,
        identity: Option<&TlsIdentity>,
    ) -> Result<Arc<NodeClient>, ScenarioError> {
        let transport = match identity {
            Some(identity) => {
                RouterTransport::presenting(self.shared.router.clone(), identity.certificate_der.clone())
            }
            None => RouterTransport::new(self.shared.router.clone()),
        };
        Ok(Arc::new(NodeClient::new(vec![Arc::new(transport)])))
    }

    fn nfs_export(&self) -> Option<NfsExport> {
        Some(NfsExport {
            server: String::from(SIMULATED_NFS_SERVER),
            export: String::from(SIMULATED_NFS_EXPORT),
        })
    }

    fn clock(&self) -> &dyn Clock {
        &self.shared.clock
    }

    fn level(&self) -> Level {
        Level::Simulated
    }
}
