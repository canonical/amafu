use std::sync::Arc;

use amafu_domain::kinds::environment::EnvironmentPhase;
use amafu_domain::kinds::juju_controller::{JujuController, JujuControllerStatus};
use amafu_domain::kinds::kubernetes_cluster::KubernetesCluster;
use amafu_domain::{Object, ObjectReference};
use amafu_interfaces::objects::{get, list, update_status};
use amafu_interfaces::{ActionError, Consistency, Store};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostObservation {
    pub controller: EnvironmentPhase,
    pub host: Option<(String, EnvironmentPhase)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostAction {
    Degrade { message: String },
    Recover,
}

#[must_use]
pub fn plan_host_health(observation: &HostObservation) -> Vec<HostAction> {
    let Some((host, phase)) = &observation.host else {
        return Vec::new();
    };
    match (observation.controller, *phase == EnvironmentPhase::Ready) {
        (EnvironmentPhase::Ready, false) => {
            vec![HostAction::Degrade { message: format!("its Kubernetes cluster {host} is not ready") }]
        }
        (EnvironmentPhase::Degraded, true) => vec![HostAction::Recover],
        _ => Vec::new(),
    }
}

pub struct HostHealthController {
    store: Arc<dyn Store>,
}

impl HostHealthController {
    #[must_use]
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }
}

fn with_phase(
    controller: &Object<JujuController>,
    phase: EnvironmentPhase,
    message: Option<String>,
) -> Option<JujuControllerStatus> {
    let mut status = controller.status.clone();
    status.phase = phase;
    status.message = message;
    (status != controller.status).then_some(status)
}

impl Controller for HostHealthController {
    type Key = ObjectReference;
    type Observation = Option<HostObservation>;
    type Action = HostAction;

    const NAME: &'static str = "juju-host-health";
    const WATCHES: &'static [WatchedKind<ObjectReference>] = &[
        WatchedKind {
            kind: "JujuController",
            mapping: KeyMapping::ObjectKey(|reference| Some(reference.clone())),
        },
        WatchedKind { kind: "KubernetesCluster", mapping: KeyMapping::EveryKey },
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

    async fn observe(&self, key: &ObjectReference) -> Result<Option<HostObservation>, ActionError> {
        let store = self.store.as_ref();
        let Some(controller) = get::<JujuController>(store, key, Consistency::Linearizable).await? else {
            return Ok(None);
        };
        let Some(cluster) = controller.specification.substrate.hosting_cluster() else {
            return Ok(None);
        };
        let reference = ObjectReference::of::<KubernetesCluster>(key.project.clone(), cluster.clone());
        let phase = get::<KubernetesCluster>(store, &reference, Consistency::Linearizable)
            .await?
            .map_or(EnvironmentPhase::Failed, |cluster| cluster.status.phase);
        Ok(Some(HostObservation {
            controller: controller.status.phase,
            host: Some((cluster.to_string(), phase)),
        }))
    }

    fn plan(&self, observation: &Option<HostObservation>) -> Vec<HostAction> {
        observation.as_ref().map(plan_host_health).unwrap_or_default()
    }

    async fn apply(&self, key: &ObjectReference, action: HostAction) -> Result<(), ActionError> {
        update_status::<JujuController>(self.store.as_ref(), key, |controller| match &action {
            HostAction::Degrade { message } => {
                with_phase(controller, EnvironmentPhase::Degraded, Some(message.clone()))
            }
            HostAction::Recover => with_phase(controller, EnvironmentPhase::Ready, None),
        })
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(controller: EnvironmentPhase, host: EnvironmentPhase) -> HostObservation {
        HostObservation { controller, host: Some((String::from("platform"), host)) }
    }

    #[test]
    fn a_controller_follows_the_health_of_its_cluster() {
        let unhealthy = observation(EnvironmentPhase::Ready, EnvironmentPhase::Degraded);
        assert_eq!(
            plan_host_health(&unhealthy),
            [HostAction::Degrade { message: String::from("its Kubernetes cluster platform is not ready") }]
        );
        assert_eq!(
            plan_host_health(&observation(EnvironmentPhase::Degraded, EnvironmentPhase::Ready)),
            [HostAction::Recover]
        );
        assert_eq!(
            plan_host_health(&observation(EnvironmentPhase::Creating, EnvironmentPhase::Degraded)),
            []
        );
        assert_eq!(
            plan_host_health(&HostObservation { controller: EnvironmentPhase::Ready, host: None }),
            []
        );
    }
}
