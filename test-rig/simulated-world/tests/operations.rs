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
}
