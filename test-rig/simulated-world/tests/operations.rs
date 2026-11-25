#[cfg(test)]
mod resuming {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use amafu_domain::kinds::operation::{
        Operation, OperationOutcome, OperationSpecification, StepSpecification, VmReference,
    };
    use amafu_domain::{Change, NodeName, Object, ObjectId, ObjectName, ObjectReference, ProjectName};
    use amafu_interfaces::objects::{apply, get};
    use amafu_interfaces::{ActionError, Consistency, Fence};
    use amafu_reconciler::{OperationContext, Progress, Step, StepRegistry, advance};
    use amafu_simulated_world::VirtualClock;
    use amafu_simulated_world::fakes::store::MemoryStore;
    use futures::executor::block_on;
    use time::OffsetDateTime;

    #[derive(Default)]
    struct Ledger {
        runs: Vec<usize>,
        undos: Vec<usize>,
        applied: BTreeMap<usize, bool>,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Behaviour {
        Succeeds,
        FailsPermanently,
        CrashesAfterEffect,
    }

    struct RecordingRegistry {
        ledger: Arc<Mutex<Ledger>>,
        store: Arc<MemoryStore>,
        steps: Vec<StepSpecification>,
        behaviours: BTreeMap<usize, Behaviour>,
    }

    struct RecordingStep {
        index: usize,
        behaviour: Behaviour,
        ledger: Arc<Mutex<Ledger>>,
        store: Arc<MemoryStore>,
    }

    impl StepRegistry for RecordingRegistry {
        fn build(&self, specification: &StepSpecification) -> Result<Box<dyn Step>, ActionError> {
            let index = self
                .steps
                .iter()
                .position(|step| step == specification)
                .ok_or_else(|| ActionError::fatal("unknown step"))?;
            Ok(Box::new(RecordingStep {
                index,
                behaviour: self.behaviours.get(&index).copied().unwrap_or(Behaviour::Succeeds),
                ledger: Arc::clone(&self.ledger),
                store: Arc::clone(&self.store),
            }))
        }
    }

    #[async_trait::async_trait]
    impl Step for RecordingStep {
        async fn is_complete(&self, _context: &OperationContext<'_>) -> Result<bool, ActionError> {
            Ok(self.ledger.lock().unwrap().applied.get(&self.index).copied().unwrap_or(false))
        }

        async fn run(&self, _context: &OperationContext<'_>) -> Result<(), ActionError> {
            let mut ledger = self.ledger.lock().unwrap();
            ledger.runs.push(self.index);
            match self.behaviour {
                Behaviour::Succeeds => {
                    ledger.applied.insert(self.index, true);
                    Ok(())
                }
                Behaviour::FailsPermanently => Err(ActionError::invalid("the destination refused the VM")),
                Behaviour::CrashesAfterEffect => {
                    ledger.applied.insert(self.index, true);
                    self.store.set_unavailable(true);
                    Ok(())
                }
            }
        }

        async fn undo(&self, _context: &OperationContext<'_>) -> Result<(), ActionError> {
            let mut ledger = self.ledger.lock().unwrap();
            ledger.undos.push(self.index);
            ledger.applied.insert(self.index, false);
            Ok(())
        }
    }

    struct Harness {
        store: Arc<MemoryStore>,
        clock: VirtualClock,
        registry: RecordingRegistry,
        ledger: Arc<Mutex<Ledger>>,
        fence: Fence,
    }

    fn vm(name: &str) -> VmReference {
        VmReference { project: ProjectName::new("project-a").unwrap(), name: ObjectName::new(name).unwrap() }
    }

    fn reference() -> ObjectReference {
        ObjectReference::of::<Operation>(None, ObjectName::new("operation-1").unwrap())
    }

    impl Harness {
        fn new(specification: OperationSpecification, behaviours: BTreeMap<usize, Behaviour>) -> Self {
            let store = Arc::new(MemoryStore::new());
            let ledger = Arc::new(Mutex::new(Ledger::default()));
            block_on(apply::<Operation>(
                store.as_ref(),
                &reference(),
                Change::Create {
                    id: ObjectId::from_uuid(uuid::Uuid::from_u128(1)),
                    name: ObjectName::new("operation-1").unwrap(),
                    project: None,
                    labels: BTreeMap::new(),
                    specification: specification.clone(),
                },
            ))
            .unwrap();
            let registry = RecordingRegistry {
                ledger: Arc::clone(&ledger),
                store: Arc::clone(&store),
                steps: specification.steps,
                behaviours,
            };
            Self {
                store,
                clock: VirtualClock::starting_at(OffsetDateTime::UNIX_EPOCH),
                registry,
                ledger,
                fence: Fence { name: String::from("operations"), token: 1 },
            }
        }

        fn operation(&self) -> Object<Operation> {
            block_on(get::<Operation>(self.store.as_ref(), &reference(), Consistency::Linearizable))
                .unwrap()
                .unwrap()
        }

        fn advance_once(&self) -> Result<Progress, ActionError> {
            let operation = self.operation();
            let context = OperationContext {
                operation: &operation,
                store: self.store.as_ref(),
                clock: &self.clock,
                fence: &self.fence,
            };
            block_on(advance(&context, &self.registry))
        }

        fn advance_until_finished(&self) -> Object<Operation> {
            for _ in 0..100 {
                self.store.set_unavailable(false);
                if self.advance_once() == Ok(Progress::Finished) {
                    return self.operation();
                }
            }
            panic!("the operation did not finish");
        }
    }

    fn evacuation_of_three() -> OperationSpecification {
        OperationSpecification::evacuation(NodeName::new("node-1").unwrap(), vec![vm("a"), vm("b"), vm("c")])
    }

    #[test]
    fn an_operation_resumes_at_the_right_step_after_a_crash_between_any_two_steps() {
        for crashing_step in 0..4 {
            let harness = Harness::new(
                evacuation_of_three(),
                BTreeMap::from([(crashing_step, Behaviour::CrashesAfterEffect)]),
            );
            let finished = harness.advance_until_finished();
            assert_eq!(
                finished.status.outcome,
                Some(OperationOutcome::Succeeded),
                "crash after step {crashing_step}"
            );
            assert_eq!(finished.status.completed_steps, 4);
            assert_eq!(
                harness.ledger.lock().unwrap().runs,
                vec![0, 1, 2, 3],
                "crash after step {crashing_step}"
            );
        }
    }

    #[test]
    fn every_finished_step_records_when_it_ran() {
        let harness = Harness::new(evacuation_of_three(), BTreeMap::new());
        let finished = harness.advance_until_finished();
        assert_eq!(finished.status.finished_steps.len(), finished.status.completed_steps);
        assert!(finished.status.finished_steps.iter().all(|step| step.finished_at >= step.started_at));
    }

    #[test]
    fn a_permanent_failure_before_the_switch_over_undoes_the_steps_in_reverse() {
        let migration = OperationSpecification::migration(vm("web-1"), None);
        let harness = Harness::new(migration, BTreeMap::from([(1, Behaviour::FailsPermanently)]));
        let finished = harness.advance_until_finished();
        assert!(matches!(finished.status.outcome, Some(OperationOutcome::Failed { .. })));
        let ledger = harness.ledger.lock().unwrap();
        assert_eq!(ledger.runs, vec![0, 1]);
        assert_eq!(ledger.undos, vec![1, 0]);
    }

    #[test]
    fn a_failure_past_the_switch_over_keeps_retrying_instead_of_rolling_back() {
        let migration = OperationSpecification::migration(vm("web-1"), None);
        let harness = Harness::new(migration, BTreeMap::from([(3, Behaviour::FailsPermanently)]));
        for _ in 0..10 {
            let _ = harness.advance_once();
        }
        let stuck = harness.operation();
        assert_eq!(stuck.status.outcome, None);
        assert_eq!(stuck.status.completed_steps, 3);
        assert!(
            stuck
                .status
                .current_step
                .as_ref()
                .is_some_and(|step| step.attempts > 0 && step.last_error.is_some())
        );
        assert_eq!(harness.ledger.lock().unwrap().undos, Vec::<usize>::new());
    }

    #[test]
    fn a_cancelled_migration_is_undone_but_a_paused_one_waits() {
        let migration = OperationSpecification::migration(vm("web-1"), None);
        let harness = Harness::new(migration.clone(), BTreeMap::new());
        harness.advance_once().unwrap();
        let mut operation = harness.operation();
        operation.specification.is_paused = true;
        block_on(apply::<Operation>(
            harness.store.as_ref(),
            &reference(),
            Change::UpdateSpecification {
                expected_generation: None,
                specification: operation.specification.clone(),
            },
        ))
        .unwrap();
        assert_eq!(harness.advance_once(), Ok(Progress::Idle));
        operation.specification.is_cancelled = true;
        block_on(apply::<Operation>(
            harness.store.as_ref(),
            &reference(),
            Change::UpdateSpecification { expected_generation: None, specification: operation.specification },
        ))
        .unwrap();
        let finished = harness.advance_until_finished();
        assert_eq!(finished.status.outcome, Some(OperationOutcome::Cancelled));
        assert_eq!(harness.ledger.lock().unwrap().undos, vec![0]);
    }

    fn cancel(harness: &Harness) {
        let mut operation = harness.operation();
        operation.specification.is_cancelled = true;
        block_on(apply::<Operation>(
            harness.store.as_ref(),
            &reference(),
            Change::UpdateSpecification { expected_generation: None, specification: operation.specification },
        ))
        .unwrap();
    }

    #[test]
    fn a_cancelled_evacuation_stops_between_vms_and_uncordons_the_node() {
        let harness = Harness::new(evacuation_of_three(), BTreeMap::new());
        harness.advance_once().unwrap();
        harness.advance_once().unwrap();
        cancel(&harness);
        let finished = harness.advance_until_finished();
        assert_eq!(finished.status.outcome, Some(OperationOutcome::Cancelled));
        let ledger = harness.ledger.lock().unwrap();
        assert_eq!(ledger.runs, vec![0, 1]);
        assert_eq!(ledger.undos, vec![1, 0]);
    }

    #[test]
    fn a_vm_that_cannot_move_fails_the_evacuation_instead_of_retrying_it() {
        let harness = Harness::new(evacuation_of_three(), BTreeMap::from([(2, Behaviour::FailsPermanently)]));
        let finished = harness.advance_until_finished();
        assert!(matches!(finished.status.outcome, Some(OperationOutcome::Failed { .. })));
        let ledger = harness.ledger.lock().unwrap();
        assert_eq!(ledger.runs, vec![0, 1, 2]);
        assert_eq!(ledger.undos, vec![2, 1, 0]);
    }

    fn upgrade_of_two_nodes() -> OperationSpecification {
        let image = amafu_domain::node_image::NodeImage {
            version: String::from("0.2.0"),
            feature_level: amafu_domain::features::FeatureLevel(1),
            digest: "d".repeat(64).parse().unwrap(),
            size_bytes: 1024,
            url: String::from("https://images.example/node-image.tar"),
            signature: "0".repeat(128),
        };
        let wave = amafu_domain::upgrade::UpgradeWave {
            nodes: vec![NodeName::new("node-1").unwrap(), NodeName::new("node-2").unwrap()],
        };
        OperationSpecification::upgrade(amafu_domain::upgrade::UpgradePlan {
            image,
            waves: vec![wave],
            health_deadline_seconds: 600,
            soak_seconds: 30,
            is_rollback: false,
        })
    }

    #[test]
    fn an_upgrade_resumes_at_the_right_step_after_a_crash_between_any_two_steps() {
        let step_count = upgrade_of_two_nodes().steps.len();
        for crashing_step in 0..step_count {
            let harness = Harness::new(
                upgrade_of_two_nodes(),
                BTreeMap::from([(crashing_step, Behaviour::CrashesAfterEffect)]),
            );
            let finished = harness.advance_until_finished();
            assert_eq!(
                finished.status.outcome,
                Some(OperationOutcome::Succeeded),
                "crash after step {crashing_step}"
            );
            assert_eq!(
                harness.ledger.lock().unwrap().runs,
                (0..step_count).collect::<Vec<_>>(),
                "crash after step {crashing_step}"
            );
        }
    }

    #[test]
    fn a_node_that_falls_back_ends_the_upgrade_without_undoing_the_nodes_before_it() {
        let upgrade = upgrade_of_two_nodes();
        let second_reboot = upgrade
            .steps
            .iter()
            .rposition(|step| matches!(step, StepSpecification::RebootIntoSlot { .. }))
            .unwrap();
        let harness = Harness::new(upgrade, BTreeMap::from([(second_reboot, Behaviour::FailsPermanently)]));
        let finished = harness.advance_until_finished();
        assert!(
            matches!(&finished.status.outcome, Some(OperationOutcome::Failed { reason }) if reason.contains("reboot-into-slot"))
        );
        assert_eq!(finished.status.completed_steps, second_reboot);
        assert_eq!(harness.ledger.lock().unwrap().undos, Vec::<usize>::new());
    }

    #[test]
    fn a_cancelled_upgrade_stops_at_the_next_step_and_leaves_upgraded_nodes_alone() {
        let upgrade = upgrade_of_two_nodes();
        let harness = Harness::new(upgrade, BTreeMap::new());
        for _ in 0..5 {
            harness.advance_once().unwrap();
        }
        let mut operation = harness.operation();
        let reached = operation.status.completed_steps;
        operation.specification.is_cancelled = true;
        block_on(apply::<Operation>(
            harness.store.as_ref(),
            &reference(),
            Change::UpdateSpecification { expected_generation: None, specification: operation.specification },
        ))
        .unwrap();
        let finished = harness.advance_until_finished();
        assert_eq!(finished.status.outcome, Some(OperationOutcome::Cancelled));
        assert_eq!(finished.status.completed_steps, reached);
        assert_eq!(harness.ledger.lock().unwrap().undos, Vec::<usize>::new());
    }
}

#[cfg(test)]
mod deadlines {
    use std::collections::BTreeMap;

    use amafu_controllers::operations::upgrade::{AwaitHealthy, RebootIntoSlot, Soak};
    use amafu_domain::kinds::node::{Node, NodePhase, NodeSpecification, NodeStatus, StoreHealth};
    use amafu_domain::kinds::operation::{Operation, OperationSpecification, StepProgress};
    use amafu_domain::{Change, NodeName, Object, ObjectId, ObjectName, ObjectReference};
    use amafu_interfaces::objects::{apply, get};
    use amafu_interfaces::{ActionError, Consistency, Fence};
    use amafu_reconciler::{OperationContext, Step};
    use amafu_simulated_world::VirtualClock;
    use amafu_simulated_world::fakes::store::MemoryStore;
    use futures::executor::block_on;
    use time::{Duration, OffsetDateTime};

    const DEADLINE_SECONDS: u64 = 600;

    fn node() -> NodeName {
        NodeName::new("node-1").unwrap()
    }

    fn create<R: amafu_domain::Resource>(store: &MemoryStore, name: &str, specification: R::Specification) {
        let name = ObjectName::new(name).unwrap();
        block_on(apply::<R>(
            store,
            &ObjectReference::of::<R>(None, name.clone()),
            Change::Create {
                id: ObjectId::from_uuid(uuid::Uuid::new_v4()),
                name,
                project: None,
                labels: BTreeMap::new(),
                specification,
            },
        ))
        .unwrap();
    }

    fn upgrade() -> OperationSpecification {
        let image = amafu_domain::node_image::NodeImage {
            version: String::from("26.11.2"),
            feature_level: amafu_domain::features::FeatureLevel(1),
            digest: "d".repeat(64).parse().unwrap(),
            size_bytes: 1024,
            url: String::from("https://images.example/node-image.tar"),
            signature: "0".repeat(128),
        };
        OperationSpecification::upgrade(amafu_domain::upgrade::UpgradePlan {
            image,
            waves: vec![amafu_domain::upgrade::UpgradeWave { nodes: vec![node()] }],
            health_deadline_seconds: DEADLINE_SECONDS,
            soak_seconds: 30,
            is_rollback: false,
        })
    }

    fn waited(step: &dyn Step, seconds: i64) -> Result<bool, ActionError> {
        waited_with_node(step, seconds, NodeStatus::default())
    }

    fn waited_with_node(step: &dyn Step, seconds: i64, status: NodeStatus) -> Result<bool, ActionError> {
        waited_at_step(step, seconds, status, 0)
    }

    fn waited_at_step(
        step: &dyn Step,
        seconds: i64,
        status: NodeStatus,
        completed_steps: usize,
    ) -> Result<bool, ActionError> {
        let store = MemoryStore::new();
        create::<Node>(&store, "node-1", NodeSpecification::default());
        let node_reference = ObjectReference::of::<Node>(None, ObjectName::new("node-1").unwrap());
        block_on(apply::<Node>(
            &store,
            &node_reference,
            Change::UpdateStatus { observed_generation: 1, status },
        ))
        .unwrap();
        create::<Operation>(&store, "upgrade-1", upgrade());
        let reference = ObjectReference::of::<Operation>(None, ObjectName::new("upgrade-1").unwrap());
        let mut operation: Object<Operation> =
            block_on(get::<Operation>(&store, &reference, Consistency::Linearizable)).unwrap().unwrap();
        operation.status.current_step = Some(StepProgress::started(OffsetDateTime::UNIX_EPOCH));
        operation.status.completed_steps = completed_steps;
        let clock = VirtualClock::starting_at(OffsetDateTime::UNIX_EPOCH + Duration::seconds(seconds));
        let fence = Fence { name: String::from("operations"), token: 1 };
        let context = OperationContext { operation: &operation, store: &store, clock: &clock, fence: &fence };
        block_on(step.is_complete(&context))
    }

    #[test]
    fn a_node_that_never_restarts_fails_the_reboot_step_after_the_deadline() {
        let step = RebootIntoSlot { node: node() };
        assert_eq!(waited(&step, 60), Ok(false));
        let failure = waited(&step, 601).unwrap_err();
        assert!(!failure.is_retryable());
        assert!(failure.to_string().contains("did not restart into"), "{failure}");
    }

    #[test]
    fn a_node_that_never_becomes_healthy_fails_after_the_same_deadline() {
        let step = AwaitHealthy { node: node() };
        assert_eq!(waited(&step, 60), Ok(false));
        assert!(waited(&step, 601).unwrap_err().to_string().contains("healthily within 600 seconds"));
    }

    #[test]
    fn a_soak_lasts_its_time_and_until_every_store_follower_has_caught_up() {
        let ready_with =
            |store| NodeStatus { phase: NodePhase::Ready, store: Some(store), ..NodeStatus::default() };
        let settled = StoreHealth { is_stable: true, ..StoreHealth::default() };
        let catching_up = StoreHealth { followers_catching_up: 1, ..settled };
        assert_eq!(waited_with_node(&Soak, 10, ready_with(settled)), Ok(false));
        assert_eq!(waited_with_node(&Soak, 31, ready_with(settled)), Ok(true));
        assert_eq!(waited_with_node(&Soak, 31, ready_with(catching_up)), Ok(false));
    }

    #[test]
    fn the_soak_after_the_last_wave_ends_at_once() {
        assert_eq!(waited_at_step(&Soak, 0, NodeStatus::default(), 7), Ok(true));
    }
}

#[cfg(test)]
mod lease_expiry_across_members {
    use std::collections::BTreeMap;

    use amafu_controllers::operations::recovery::MarkNodeLost;
    use amafu_domain::kinds::node_lease::{LeaseTerms, NodeLease, NodeLeaseSpecification};
    use amafu_domain::kinds::operation::{Operation, OperationSpecification, StepProgress};
    use amafu_domain::{Change, NodeName, Object, ObjectId, ObjectName, ObjectReference};
    use amafu_interfaces::objects::{apply, get};
    use amafu_interfaces::{ActionError, Consistency, Fence};
    use amafu_reconciler::{OperationContext, Step};
    use amafu_simulated_world::VirtualClock;
    use amafu_simulated_world::fakes::store::MemoryStore;
    use futures::executor::block_on;
    use time::{Duration, OffsetDateTime};

    fn node() -> NodeName {
        NodeName::new("node-1").unwrap()
    }

    fn at(seconds: i64) -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::seconds(seconds)
    }

    fn create<R: amafu_domain::Resource>(store: &MemoryStore, name: &str, specification: R::Specification) {
        let name = ObjectName::new(name).unwrap();
        block_on(apply::<R>(
            store,
            &ObjectReference::of::<R>(None, name.clone()),
            Change::Create {
                id: ObjectId::from_uuid(uuid::Uuid::new_v4()),
                name,
                project: None,
                labels: BTreeMap::new(),
                specification,
            },
        ))
        .unwrap();
    }

    fn mark_lost(renewed_at: i64, step_started_at: i64, member_clock: i64) -> Result<(), ActionError> {
        let store = MemoryStore::new();
        let terms = LeaseTerms::new(std::time::Duration::from_secs(8), std::time::Duration::from_secs(8));
        create::<NodeLease>(
            &store,
            "node-1",
            NodeLeaseSpecification {
                epoch: 7,
                renewed_at: at(renewed_at),
                terms,
                is_lost: false,
                is_released: false,
            },
        );
        create::<Operation>(&store, "recovery-node-1-7", OperationSpecification::recovery(node()));
        let reference = ObjectReference::of::<Operation>(None, ObjectName::new("recovery-node-1-7").unwrap());
        let mut operation: Object<Operation> =
            block_on(get::<Operation>(&store, &reference, Consistency::Linearizable)).unwrap().unwrap();
        operation.status.current_step = Some(StepProgress::started(at(step_started_at)));
        let clock = VirtualClock::starting_at(at(member_clock));
        let fence = Fence { name: String::from("operations"), token: 1 };
        let context = OperationContext { operation: &operation, store: &store, clock: &clock, fence: &fence };
        block_on(MarkNodeLost { node: node() }.run(&context))
    }

    #[test]
    fn a_member_whose_clock_lags_waits_instead_of_failing_the_recovery() {
        let refusal = mark_lost(100, 109, 105).unwrap_err();
        assert!(refusal.is_retryable(), "{refusal}");
    }

    #[test]
    fn a_lease_renewed_after_the_recovery_started_proves_the_node_alive() {
        let refusal = mark_lost(110, 109, 111).unwrap_err();
        assert!(!refusal.is_retryable(), "{refusal}");
    }

    #[test]
    fn an_expired_lease_is_marked_lost() {
        assert_eq!(mark_lost(100, 109, 120), Ok(()));
    }
}
