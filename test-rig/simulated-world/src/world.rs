use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::net::Ipv4Addr;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use amafu_api::authentication::TokenIdentity;
use amafu_api::{ApiClient, ApiState, Principal, RouterTransport, TokenAuthenticator, router};
use amafu_checkers::invariants::check_all;
use amafu_controllers::{
    ComputeController, ComputeServices, ImageController, NetworkController, NodeController, NodeIdentity,
    NodeNetworkController, PlacementController, SecurityGroupController, StorageBackendController,
    VolumeController,
};
use amafu_domain::access::Role;
use amafu_domain::kinds::node::{NodeCapacity, NodeRole};
use amafu_domain::{MacAddress, NodeName, ObjectId, ObjectName, ProjectName};
use amafu_interfaces::{
    AuditLog, Clock, Datapath, GuestAgents, Hypervisor, Identifiers, ImageStore, LocalServices,
    NodeNetworkState, Storage, Store, WorkloadState,
};
use amafu_network::dhcp::{self, DhcpMessage, Lease, MessageType};
use amafu_reconciler::{Controller, ReconcilerSettings, run_controller};
use amafu_scenario::{
    Cluster, ClusterObservation, Fault, FaultHandle, Level, ObservedAttachment, ObservedInstance,
    ObservedPort, ScenarioDefinition, ScenarioError, ScenarioResult,
};
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::clock::VirtualClock;
use crate::executor::{Executor, StepOutcome, TaskFuture, TaskId};
use crate::fakes::{
    FakeBackends, FakeDatapath, FakeHypervisor, FakeImageStore, FakeLocalServices, FakeStorage,
    MemoryAuditLog, MemoryStore, SeededIdentifiers,
};

const PROJECTS: [&str; 2] = ["project-a", "project-b"];
const START_UNIX_SECONDS: i64 = 1_767_225_600;
const RECENT_EVENT_COUNT: usize = 200;
const ROLE_CRASH_PROBABILITY: f64 = 0.02;
const BOOT_DURATION: Duration = Duration::from_secs(1);
const TEST_BUNDLE: &str = "test-guest";

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
    hypervisor: Arc<FakeHypervisor>,
    storage: Arc<FakeStorage>,
    images: Arc<FakeImageStore>,
    datapath: Arc<FakeDatapath>,
    local_services: Arc<FakeLocalServices>,
}

impl NodeServices {
    fn compute(&self, store: &Arc<dyn Store>) -> ComputeServices {
        ComputeServices {
            store: Arc::clone(store),
            hypervisor: Arc::clone(&self.hypervisor) as Arc<dyn Hypervisor>,
            storage: Arc::clone(&self.storage) as Arc<dyn Storage>,
            images: Arc::clone(&self.images) as Arc<dyn ImageStore>,
            datapath: Arc::clone(&self.datapath) as Arc<dyn Datapath>,
            agents: Arc::clone(&self.hypervisor) as Arc<dyn GuestAgents>,
        }
    }

    fn power_off(&self) {
        self.hypervisor.power_off();
        self.storage.power_off();
        self.datapath.power_off();
        self.local_services.power_off();
    }
}

struct SimulatedNode {
    services: NodeServices,
    roles: Vec<RoleTask>,
}

struct WorldShared {
    seed: u64,
    crashes_roles: bool,
    random: RefCell<ChaCha8Rng>,
    executor: Executor,
    clock: VirtualClock,
    store: Arc<MemoryStore>,
    backends: Arc<FakeBackends>,
    audit: Arc<MemoryAuditLog>,
    router: axum::Router,
    tokens: BTreeMap<(Role, ProjectName), String>,
    expired_token: String,
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
        let router = router(ApiState {
            store: Arc::clone(&store) as Arc<dyn Store>,
            authenticator: Arc::new(TokenAuthenticator::new(identities)),
            audit: Arc::clone(&audit) as Arc<dyn AuditLog>,
            clock: Arc::new(clock.clone()),
            identifiers: Arc::clone(&identifiers) as Arc<dyn Identifiers>,
        });
        let world = Self {
            shared: Rc::new(WorldShared {
                seed: settings.seed,
                crashes_roles: settings.crashes_roles,
                random: RefCell::new(ChaCha8Rng::seed_from_u64(settings.seed)),
                executor: Executor::new(clock.clone()),
                clock,
                store,
                backends: FakeBackends::new(),
                audit,
                router,
                tokens,
                expired_token,
                nodes: RefCell::new(BTreeMap::new()),
                failures: Rc::new(RefCell::new(Vec::new())),
                recent_events: RefCell::new(VecDeque::new()),
                next_fault: Cell::new(0),
            }),
        };
        for index in 0..node_count {
            world.add_node(&valid_name(&format!("node-{index}")), index == 0, identifiers.as_ref());
        }
        world
    }

    fn role<C: Controller>(
        &self,
        label: String,
        settings: ReconcilerSettings,
        make: impl Fn() -> C + 'static,
    ) -> RoleTask {
        let shared = &self.shared;
        controller_role(
            label,
            make,
            Arc::clone(&shared.store) as Arc<dyn Store>,
            Arc::new(shared.clock.clone()),
            settings,
            Rc::clone(&shared.failures),
        )
    }

    fn add_node(&self, name: &NodeName, is_core: bool, identifiers: &dyn Identifiers) {
        let shared = &self.shared;
        let clock: Arc<dyn Clock> = Arc::new(shared.clock.clone());
        let store: Arc<dyn Store> = Arc::clone(&shared.store) as Arc<dyn Store>;
        let bundles = BTreeSet::from([valid_name::<ObjectName>(TEST_BUNDLE)]);
        let services = NodeServices {
            hypervisor: Arc::new(FakeHypervisor::new(Arc::clone(&clock), BOOT_DURATION, bundles)),
            storage: Arc::new(FakeStorage::new(Arc::clone(&shared.backends))),
            images: Arc::new(FakeImageStore::new()),
            datapath: Arc::new(FakeDatapath::new()),
            local_services: Arc::new(FakeLocalServices::new()),
        };
        let mut roles = BTreeSet::from([NodeRole::Compute]);
        if is_core {
            roles.insert(NodeRole::Core);
        }
        let identity = NodeIdentity {
            name: name.clone(),
            id: ObjectId::from_uuid(identifiers.generate()),
            roles,
            capacity: NodeCapacity { cpu_count: 16, memory_mebibytes: 65_536 },
            software_version: String::from(env!("CARGO_PKG_VERSION")),
            heartbeat_interval: Duration::from_secs(10),
        };
        let fast = ReconcilerSettings {
            initial_backoff: Duration::from_millis(100),
            maximum_backoff: Duration::from_secs(5),
            periodic_resynchronization: Duration::from_secs(2),
        };
        let heartbeat = ReconcilerSettings { periodic_resynchronization: Duration::from_secs(5), ..fast };
        let compute = services.compute(&store);
        let (network_store, datapath, local_services) =
            (Arc::clone(&store), Arc::clone(&services.datapath), Arc::clone(&services.local_services));
        let (node_store, node_clock, node_name) = (Arc::clone(&store), Arc::clone(&clock), name.clone());
        let mut tasks = vec![
            self.role(format!("{name}/node"), heartbeat, move || {
                NodeController::new(identity.clone(), Arc::clone(&node_store), Arc::clone(&node_clock))
            }),
            self.role(format!("{name}/compute"), fast, move || {
                ComputeController::new(node_name.clone(), compute.clone())
            }),
        ];
        let node_name = name.clone();
        tasks.push(self.role(format!("{name}/node-network"), fast, move || {
            NodeNetworkController::new(
                node_name.clone(),
                Arc::clone(&network_store),
                Arc::clone(&datapath) as Arc<dyn Datapath>,
                Arc::clone(&local_services) as Arc<dyn LocalServices>,
            )
        }));
        if is_core {
            tasks.extend(self.core_roles(name, &store, &services, fast));
        }
        for role in &tasks {
            role.task.set(shared.executor.spawn(role.label.clone(), (role.start)()));
        }
        shared.nodes.borrow_mut().insert(name.clone(), SimulatedNode { services, roles: tasks });
    }

    fn core_roles(
        &self,
        name: &NodeName,
        store: &Arc<dyn Store>,
        services: &NodeServices,
        settings: ReconcilerSettings,
    ) -> Vec<RoleTask> {
        let storage: Arc<dyn Storage> = Arc::clone(&services.storage) as Arc<dyn Storage>;
        let images: Arc<dyn ImageStore> = Arc::clone(&services.images) as Arc<dyn ImageStore>;
        let (placement_store, network_store, group_store) =
            (Arc::clone(store), Arc::clone(store), Arc::clone(store));
        let (volume_store, volume_storage, volume_images) =
            (Arc::clone(store), Arc::clone(&storage), Arc::clone(&images));
        let (image_store, image_cache, image_node) = (Arc::clone(store), Arc::clone(&images), name.clone());
        let (backend_store, backend_storage) = (Arc::clone(store), Arc::clone(&storage));
        vec![
            self.role(format!("{name}/placement"), settings, move || {
                PlacementController::new(Arc::clone(&placement_store))
            }),
            self.role(format!("{name}/network"), settings, move || {
                NetworkController::new(Arc::clone(&network_store))
            }),
            self.role(format!("{name}/security-group"), settings, move || {
                SecurityGroupController::new(Arc::clone(&group_store))
            }),
            self.role(format!("{name}/volume"), settings, move || {
                VolumeController::new(
                    Arc::clone(&volume_store),
                    Arc::clone(&volume_storage),
                    Arc::clone(&volume_images),
                )
            }),
            self.role(format!("{name}/image"), settings, move || {
                ImageController::new(image_node.clone(), Arc::clone(&image_store), Arc::clone(&image_cache))
            }),
            self.role(format!("{name}/storage-backend"), settings, move || {
                StorageBackendController::new(Arc::clone(&backend_store), Arc::clone(&backend_storage))
            }),
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

    fn maybe_crash_a_role(&self) {
        if !self.shared.crashes_roles || !self.shared.random.borrow_mut().random_bool(ROLE_CRASH_PROBABILITY)
        {
            return;
        }
        let nodes = self.shared.nodes.borrow();
        let roles: Vec<&RoleTask> = nodes.values().flat_map(|node| node.roles.iter()).collect();
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
        let mut observation = ClusterObservation {
            stored_objects: self.shared.store.all_objects(),
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
                });
            }
            for (target, path) in services.storage.attachments() {
                observation.attachments.push(ObservedAttachment {
                    node: name.clone(),
                    volume: format!("{target:?}"),
                    path,
                });
            }
            for port in services.datapath.state().ports {
                observation.ports.push(ObservedPort {
                    node: name.clone(),
                    vm_id: port.key.vm_id,
                    segment: port.segment.to_string(),
                    address: port.address,
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
        outcome
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
        }
        let handle = FaultHandle(self.shared.next_fault.get());
        self.shared.next_fault.set(handle.0 + 1);
        Ok(handle)
    }

    async fn heal(&self, _handle: FaultHandle) -> Result<(), ScenarioError> {
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

    fn clock(&self) -> &dyn Clock {
        &self.shared.clock
    }

    fn level(&self) -> Level {
        Level::Simulated
    }
}
