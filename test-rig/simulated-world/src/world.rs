use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use amafu_api::authentication::TokenIdentity;
use amafu_api::{ApiClient, ApiState, Principal, RouterTransport, TokenAuthenticator, router};
use amafu_checkers::invariants::check_all;
use amafu_controllers::{ComputeController, NodeController, NodeIdentity, PlacementController};
use amafu_domain::access::Role;
use amafu_domain::kinds::node::{NodeCapacity, NodeRole};
use amafu_domain::{NodeName, ObjectId, ObjectName, ProjectName};
use amafu_interfaces::{AuditLog, Clock, Identifiers, Store};
use amafu_reconciler::{Controller, ReconcilerSettings, run_controller};
use amafu_scenario::{
    Cluster, ClusterObservation, Fault, FaultHandle, Level, ObservedInstance, ScenarioDefinition,
    ScenarioError, ScenarioResult,
};
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::clock::VirtualClock;
use crate::executor::{Executor, StepOutcome, TaskFuture, TaskId};
use crate::fakes::{FakeHypervisor, MemoryAuditLog, MemoryStore, SeededIdentifiers};

const PROJECTS: [&str; 2] = ["project-a", "project-b"];
const START_UNIX_SECONDS: i64 = 1_767_225_600;
const RECENT_EVENT_COUNT: usize = 200;
const ROLE_CRASH_PROBABILITY: f64 = 0.02;
const BOOT_DURATION: Duration = Duration::from_secs(1);
const TEST_IMAGE: &str = "test-guest";

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

struct SimulatedNode {
    hypervisor: Arc<FakeHypervisor>,
    roles: Vec<RoleTask>,
}

struct WorldShared {
    seed: u64,
    crashes_roles: bool,
    random: RefCell<ChaCha8Rng>,
    executor: Executor,
    clock: VirtualClock,
    store: Arc<MemoryStore>,
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

    fn add_node(&self, name: &NodeName, is_core: bool, identifiers: &dyn Identifiers) {
        let shared = &self.shared;
        let clock: Arc<dyn Clock> = Arc::new(shared.clock.clone());
        let store: Arc<dyn Store> = Arc::clone(&shared.store) as Arc<dyn Store>;
        let images = BTreeSet::from([valid_name::<ObjectName>(TEST_IMAGE)]);
        let hypervisor = Arc::new(FakeHypervisor::new(Arc::clone(&clock), BOOT_DURATION, images));
        let failures = &shared.failures;
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
        let mut tasks = Vec::new();
        {
            let (store_for_role, clock_for_role) = (Arc::clone(&store), Arc::clone(&clock));
            tasks.push(controller_role(
                format!("{name}/node"),
                move || {
                    NodeController::new(
                        identity.clone(),
                        Arc::clone(&store_for_role),
                        Arc::clone(&clock_for_role),
                    )
                },
                Arc::clone(&store),
                Arc::clone(&clock),
                heartbeat,
                Rc::clone(failures),
            ));
        }
        {
            let (node, store_for_role, hypervisor_for_role) =
                (name.clone(), Arc::clone(&store), Arc::clone(&hypervisor));
            tasks.push(controller_role(
                format!("{name}/compute"),
                move || {
                    ComputeController::new(
                        node.clone(),
                        Arc::clone(&store_for_role),
                        Arc::clone(&hypervisor_for_role) as Arc<dyn amafu_interfaces::Hypervisor>,
                    )
                },
                Arc::clone(&store),
                Arc::clone(&clock),
                fast,
                Rc::clone(failures),
            ));
        }
        if is_core {
            let store_for_role = Arc::clone(&store);
            tasks.push(controller_role(
                format!("{name}/placement"),
                move || PlacementController::new(Arc::clone(&store_for_role)),
                Arc::clone(&store),
                Arc::clone(&clock),
                fast,
                Rc::clone(failures),
            ));
        }
        for role in &tasks {
            role.task.set(shared.executor.spawn(role.label.clone(), (role.start)()));
        }
        shared.nodes.borrow_mut().insert(name.clone(), SimulatedNode { hypervisor, roles: tasks });
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
        simulated.hypervisor.power_off();
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
        let instances = self
            .shared
            .nodes
            .borrow()
            .iter()
            .flat_map(|(name, node)| {
                node.hypervisor.instances().into_iter().map(|instance| ObservedInstance {
                    node: name.clone(),
                    vm_id: instance.vm_id,
                    state: instance.state,
                })
            })
            .collect();
        ClusterObservation { instances, stored_objects: self.shared.store.all_objects() }
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
