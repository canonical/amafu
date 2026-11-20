use std::marker::PhantomData;
use std::sync::Arc;

use amafu_domain::kinds::environment::{EnvironmentChange, EnvironmentPhase, EnvironmentStatus};
use amafu_domain::kinds::juju_controller::{JujuController, JujuControllerChange};
use amafu_domain::kinds::kubernetes_cluster::{KubernetesCluster, KubernetesClusterChange};
use amafu_domain::kinds::operation::{OperationOutcome, OperationStatus};
use amafu_domain::{Object, ObjectReference, Resource};
use amafu_interfaces::objects::{get, list, update_status};
use amafu_interfaces::{ActionError, Consistency, Store};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};

pub trait EnvironmentKinds: Send + Sync + 'static {
    type Environment: Resource<Status: EnvironmentStatus>;
    type Change: Resource<Status = OperationStatus, Specification: EnvironmentChange>;
    const NAME: &'static str;
    const WATCHES: &'static [WatchedKind<ObjectReference>];
}

pub struct KubernetesKinds;

impl EnvironmentKinds for KubernetesKinds {
    type Environment = KubernetesCluster;
    type Change = KubernetesClusterChange;
    const NAME: &'static str = "kubernetes-outcomes";
    const WATCHES: &'static [WatchedKind<ObjectReference>] = &[
        WatchedKind {
            kind: "KubernetesCluster",
            mapping: KeyMapping::ObjectKey(|reference| Some(reference.clone())),
        },
        WatchedKind { kind: "KubernetesClusterChange", mapping: KeyMapping::EveryKey },
    ];
}

pub struct JujuKinds;

impl EnvironmentKinds for JujuKinds {
    type Environment = JujuController;
    type Change = JujuControllerChange;
    const NAME: &'static str = "juju-outcomes";
    const WATCHES: &'static [WatchedKind<ObjectReference>] = &[
        WatchedKind {
            kind: "JujuController",
            mapping: KeyMapping::ObjectKey(|reference| Some(reference.clone())),
        },
        WatchedKind { kind: "JujuControllerChange", mapping: KeyMapping::EveryKey },
    ];
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreationObservation {
    pub phase: EnvironmentPhase,
    pub failure: Option<String>,
    pub is_changing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreationAction {
    MarkFailed { reason: String },
}

#[must_use]
pub fn plan_creation(observation: &CreationObservation) -> Vec<CreationAction> {
    match (&observation.failure, observation.phase, observation.is_changing) {
        (Some(reason), EnvironmentPhase::Creating, false) => {
            vec![CreationAction::MarkFailed { reason: reason.clone() }]
        }
        _ => Vec::new(),
    }
}

fn failure_of<C: Resource<Status = OperationStatus>>(change: &Object<C>) -> Option<String> {
    match &change.status.outcome {
        Some(OperationOutcome::Failed { reason }) => Some(reason.clone()),
        Some(OperationOutcome::Cancelled) => Some(String::from("its creation was cancelled")),
        Some(OperationOutcome::Succeeded) | None => None,
    }
}

pub struct CreationOutcomeController<K: EnvironmentKinds> {
    store: Arc<dyn Store>,
    kinds: PhantomData<K>,
}

impl<K: EnvironmentKinds> CreationOutcomeController<K> {
    #[must_use]
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store, kinds: PhantomData }
    }
}

impl<K: EnvironmentKinds> Controller for CreationOutcomeController<K> {
    type Key = ObjectReference;
    type Observation = Option<CreationObservation>;
    type Action = CreationAction;

    const NAME: &'static str = K::NAME;
    const WATCHES: &'static [WatchedKind<ObjectReference>] = K::WATCHES;

    async fn discover_keys(&self) -> Result<Vec<ObjectReference>, ActionError> {
        let environments =
            list::<K::Environment>(self.store.as_ref(), None, Consistency::Linearizable).await?;
        Ok(environments
            .iter()
            .filter(|environment| environment.status.phase() == EnvironmentPhase::Creating)
            .map(Object::reference)
            .collect())
    }

    async fn observe(&self, key: &ObjectReference) -> Result<Option<CreationObservation>, ActionError> {
        let store = self.store.as_ref();
        let Some(environment) = get::<K::Environment>(store, key, Consistency::Linearizable).await? else {
            return Ok(None);
        };
        let changes: Vec<_> = list::<K::Change>(store, key.project.as_ref(), Consistency::Linearizable)
            .await?
            .into_iter()
            .filter(|change| change.specification.environment() == &key.name)
            .collect();
        let is_changing = changes.iter().any(|change| !change.status.is_finished());
        let failure =
            changes.iter().filter(|change| change.specification.creates_environment()).find_map(failure_of);
        Ok(Some(CreationObservation { phase: environment.status.phase(), failure, is_changing }))
    }

    fn plan(&self, observation: &Option<CreationObservation>) -> Vec<CreationAction> {
        observation.as_ref().map(plan_creation).unwrap_or_default()
    }

    async fn apply(&self, key: &ObjectReference, action: CreationAction) -> Result<(), ActionError> {
        let CreationAction::MarkFailed { reason } = action;
        update_status::<K::Environment>(self.store.as_ref(), key, |environment| {
            (environment.status.phase() == EnvironmentPhase::Creating)
                .then(|| environment.status.failed(&reason))
        })
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(phase: EnvironmentPhase, failure: Option<&str>, is_changing: bool) -> CreationObservation {
        CreationObservation { phase, failure: failure.map(str::to_owned), is_changing }
    }

    #[test]
    fn an_environment_whose_creation_failed_says_so_and_why() {
        assert_eq!(
            plan_creation(&observation(EnvironmentPhase::Creating, Some("no image"), false)),
            [CreationAction::MarkFailed { reason: String::from("no image") }]
        );
        assert_eq!(plan_creation(&observation(EnvironmentPhase::Creating, None, false)), []);
        assert_eq!(plan_creation(&observation(EnvironmentPhase::Creating, Some("no image"), true)), []);
        assert_eq!(plan_creation(&observation(EnvironmentPhase::Ready, Some("no image"), false)), []);
    }
}
