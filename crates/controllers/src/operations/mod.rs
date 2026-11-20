pub mod migration;
pub mod node_specification;
pub mod recovery;
pub mod steps;
pub mod upgrade;

use std::sync::Arc;
use std::time::Duration;

use amafu_domain::kinds::operation::Operation;
use amafu_domain::step_plan::Runnable;
use amafu_domain::{Object, ObjectReference};
use amafu_interfaces::objects::{get, list, remove};
use amafu_interfaces::{ActionError, Clock, Consistency, Fence, Store};
use amafu_reconciler::{
    Controller, KeyMapping, OperationContext, Progress, StepRegistry, WatchedKind, advance,
};

const PROGRESS_POLL: Duration = Duration::from_millis(250);
const MAXIMUM_STEPS_PER_PASS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationObservation {
    Absent,
    Running,
    Finished { is_deleting: bool },
    Abandoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationAction {
    Advance,
    Remove,
}

#[must_use]
pub fn observe_operation<K: Runnable>(operation: Option<&Object<K>>) -> OperationObservation {
    match operation {
        None => OperationObservation::Absent,
        Some(operation) if operation.status.is_finished() => {
            OperationObservation::Finished { is_deleting: operation.metadata.is_deleting }
        }
        Some(operation)
            if operation.metadata.is_deleting
                && operation.status.completed_steps == 0
                && operation.status.current_step.is_none() =>
        {
            OperationObservation::Abandoned
        }
        Some(_) => OperationObservation::Running,
    }
}

#[must_use]
pub fn plan_operation(observation: OperationObservation) -> Vec<OperationAction> {
    match observation {
        OperationObservation::Running => vec![OperationAction::Advance],
        OperationObservation::Finished { is_deleting: true } | OperationObservation::Abandoned => {
            vec![OperationAction::Remove]
        }
        OperationObservation::Absent | OperationObservation::Finished { is_deleting: false } => Vec::new(),
    }
}

pub trait RunnerKind: Runnable {
    const RUNNER: &'static str;
    const WATCHES: &'static [WatchedKind<ObjectReference>];
}

const fn watching(kind: &'static str) -> [WatchedKind<ObjectReference>; 1] {
    [WatchedKind { kind, mapping: KeyMapping::ObjectKey(|reference| Some(reference.clone())) }]
}

impl RunnerKind for Operation {
    const RUNNER: &'static str = "operations";
    const WATCHES: &'static [WatchedKind<ObjectReference>] = &watching("Operation");
}

pub struct OperationsController<K: RunnerKind = Operation> {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    steps: Arc<dyn StepRegistry<K>>,
    fence: Fence,
}

impl<K: RunnerKind> OperationsController<K> {
    #[must_use]
    pub fn new(
        store: Arc<dyn Store>,
        clock: Arc<dyn Clock>,
        steps: Arc<dyn StepRegistry<K>>,
        fence: Fence,
    ) -> Self {
        Self { store, clock, steps, fence }
    }

    async fn advance_until_waiting(&self, key: &ObjectReference) -> Result<(), ActionError> {
        for _ in 0..MAXIMUM_STEPS_PER_PASS {
            let Some(operation) =
                get::<K>(self.store.as_ref(), key, Consistency::Linearizable).await?
            else {
                return Ok(());
            };
            let context = OperationContext {
                operation: &operation,
                store: self.store.as_ref(),
                clock: self.clock.as_ref(),
                fence: &self.fence,
            };
            match advance(&context, self.steps.as_ref()).await? {
                Progress::Advanced => {
                    tracing::info!(runner = K::RUNNER, operation = %key, "a step plan advanced");
                }
                Progress::Idle | Progress::Waiting | Progress::Finished => return Ok(()),
            }
        }
        Ok(())
    }
}

impl<K: RunnerKind> Controller for OperationsController<K> {
    type Key = ObjectReference;
    type Observation = OperationObservation;
    type Action = OperationAction;

    const NAME: &'static str = K::RUNNER;
    const WATCHES: &'static [WatchedKind<ObjectReference>] = K::WATCHES;

    async fn discover_keys(&self) -> Result<Vec<ObjectReference>, ActionError> {
        let operations = list::<K>(self.store.as_ref(), None, Consistency::Linearizable).await?;
        Ok(operations.iter().map(Object::reference).collect())
    }

    async fn observe(&self, key: &ObjectReference) -> Result<OperationObservation, ActionError> {
        let operation = get::<K>(self.store.as_ref(), key, Consistency::Linearizable).await?;
        Ok(observe_operation(operation.as_ref()))
    }

    fn plan(&self, observation: &OperationObservation) -> Vec<OperationAction> {
        plan_operation(*observation)
    }

    async fn apply(&self, key: &ObjectReference, action: OperationAction) -> Result<(), ActionError> {
        match action {
            OperationAction::Advance => self.advance_until_waiting(key).await,
            OperationAction::Remove => Ok(remove::<K>(self.store.as_ref(), key).await?),
        }
    }

    fn revisit_after(&self, observation: &OperationObservation) -> Option<Duration> {
        (*observation == OperationObservation::Running).then_some(PROGRESS_POLL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_operations_advance_and_finished_ones_are_removed_only_when_deleted() {
        assert_eq!(plan_operation(OperationObservation::Running), vec![OperationAction::Advance]);
        assert_eq!(plan_operation(OperationObservation::Finished { is_deleting: false }), Vec::new());
        assert_eq!(
            plan_operation(OperationObservation::Finished { is_deleting: true }),
            vec![OperationAction::Remove]
        );
        assert_eq!(plan_operation(OperationObservation::Abandoned), vec![OperationAction::Remove]);
        assert_eq!(plan_operation(OperationObservation::Absent), Vec::new());
    }
}
