use std::collections::BTreeMap;
use std::sync::Arc;

use amafu_domain::kinds::environment::EnvironmentPhase;
use amafu_domain::kinds::juju_controller::{
    JujuController, JujuControllerChange, JujuControllerChangeSpecification, JujuRequest,
};
use amafu_domain::{Change, Object, ObjectId, ObjectName, ObjectReference};
use amafu_interfaces::objects::{apply, get, list};
use amafu_interfaces::{ActionError, Clock, Consistency, Identifiers, Store};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};
use time::{Duration, OffsetDateTime};

pub const ROTATION_INTERVAL: Duration = Duration::days(30);
const RECHECK: std::time::Duration = std::time::Duration::from_secs(3600);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotationObservation {
    pub is_ready: bool,
    pub is_changing: bool,
    pub rotated_at: Option<OffsetDateTime>,
    pub now: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationAction {
    Rotate,
}

#[must_use]
pub fn plan_rotation(observation: &RotationObservation) -> Vec<RotationAction> {
    let is_due =
        observation.rotated_at.is_some_and(|rotated_at| observation.now - rotated_at >= ROTATION_INTERVAL);
    if observation.is_ready && !observation.is_changing && is_due {
        vec![RotationAction::Rotate]
    } else {
        Vec::new()
    }
}

pub struct CredentialRotationController {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    identifiers: Arc<dyn Identifiers>,
}

impl CredentialRotationController {
    #[must_use]
    pub fn new(store: Arc<dyn Store>, clock: Arc<dyn Clock>, identifiers: Arc<dyn Identifiers>) -> Self {
        Self { store, clock, identifiers }
    }

    fn change_name(&self, controller: &ObjectName) -> Result<ObjectName, ActionError> {
        let tag: String = self.identifiers.generate().simple().to_string().chars().take(8).collect();
        ObjectName::new(format!("rotate-credential-{controller}-{tag}")).map_err(ActionError::fatal)
    }
}

impl Controller for CredentialRotationController {
    type Key = ObjectReference;
    type Observation = Option<RotationObservation>;
    type Action = RotationAction;

    const NAME: &'static str = "juju-credential-rotation";
    const WATCHES: &'static [WatchedKind<ObjectReference>] = &[
        WatchedKind {
            kind: "JujuController",
            mapping: KeyMapping::ObjectKey(|reference| Some(reference.clone())),
        },
        WatchedKind { kind: "JujuControllerChange", mapping: KeyMapping::EveryKey },
    ];

    async fn discover_keys(&self) -> Result<Vec<ObjectReference>, ActionError> {
        let controllers =
            list::<JujuController>(self.store.as_ref(), None, Consistency::Linearizable).await?;
        Ok(controllers
            .iter()
            .filter(|controller| controller.specification.substrate.hosting_cluster().is_some())
            .map(Object::reference)
            .collect())
    }

    async fn observe(&self, key: &ObjectReference) -> Result<Option<RotationObservation>, ActionError> {
        let store = self.store.as_ref();
        let Some(controller) = get::<JujuController>(store, key, Consistency::Linearizable).await? else {
            return Ok(None);
        };
        let changes =
            list::<JujuControllerChange>(store, key.project.as_ref(), Consistency::Linearizable).await?;
        let is_changing = changes
            .iter()
            .any(|change| change.specification.controller == key.name && !change.status.is_finished());
        Ok(Some(RotationObservation {
            is_ready: controller.status.phase == EnvironmentPhase::Ready,
            is_changing,
            rotated_at: controller.status.credential_rotated_at,
            now: self.clock.now(),
        }))
    }

    fn plan(&self, observation: &Option<RotationObservation>) -> Vec<RotationAction> {
        observation.as_ref().map(plan_rotation).unwrap_or_default()
    }

    async fn apply(&self, key: &ObjectReference, _action: RotationAction) -> Result<(), ActionError> {
        let name = self.change_name(&key.name)?;
        let reference = ObjectReference::of::<JujuControllerChange>(key.project.clone(), name.clone());
        let specification =
            JujuControllerChangeSpecification::new(key.name.clone(), JujuRequest::RotateCredential);
        let change = Change::Create {
            id: ObjectId::from_uuid(self.identifiers.generate()),
            name,
            project: key.project.clone(),
            labels: BTreeMap::new(),
            specification,
        };
        tracing::info!(controller = %key, "rotating a Juju controller's cluster credential");
        apply::<JujuControllerChange>(self.store.as_ref(), &reference, change).await?;
        Ok(())
    }

    fn revisit_after(&self, observation: &Option<RotationObservation>) -> Option<std::time::Duration> {
        observation.as_ref().map(|_| RECHECK)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(age: Option<Duration>, is_changing: bool) -> RotationObservation {
        let now = OffsetDateTime::UNIX_EPOCH + Duration::days(20_000);
        RotationObservation { is_ready: true, is_changing, rotated_at: age.map(|age| now - age), now }
    }

    #[test]
    fn a_credential_is_rotated_monthly_when_nothing_else_runs() {
        assert_eq!(plan_rotation(&observation(Some(Duration::days(31)), false)), [RotationAction::Rotate]);
        assert_eq!(plan_rotation(&observation(Some(Duration::days(3)), false)), []);
        assert_eq!(plan_rotation(&observation(Some(Duration::days(31)), true)), []);
        assert_eq!(plan_rotation(&observation(None, false)), []);
    }
}
