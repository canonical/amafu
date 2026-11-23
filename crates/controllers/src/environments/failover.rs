use std::sync::Arc;

use amafu_domain::kinds::environment::EnvironmentPhase;
use amafu_domain::kinds::floating_ip::{FloatingIp, FloatingIpSpecification, FloatingIpTarget};
use amafu_domain::kinds::kubernetes_cluster::{KubernetesCluster, KubernetesClusterChange};
use amafu_domain::kinds::vm::{Vm, VmPhase};
use amafu_domain::{Change, ObjectName, ObjectReference};
use amafu_interfaces::objects::{apply, get, list};
use amafu_interfaces::{ActionError, Consistency, Store};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};

use super::kubernetes::ownership_of;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiAddressObservation {
    pub address: ObjectReference,
    pub address_generation: u64,
    pub target: Option<ObjectName>,
    pub control_plane: Vec<(ObjectName, VmPhase)>,
    pub is_changing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiAddressAction {
    Retarget { address: ObjectReference, expected_generation: u64, vm: ObjectName },
}

#[must_use]
pub fn plan_api_address(observation: &ApiAddressObservation) -> Vec<ApiAddressAction> {
    if observation.is_changing {
        return Vec::new();
    }
    let is_running = |name: &ObjectName| {
        observation.control_plane.iter().any(|(vm, phase)| vm == name && *phase == VmPhase::Running)
    };
    if observation.target.as_ref().is_some_and(is_running) {
        return Vec::new();
    }
    let Some((successor, _)) = observation.control_plane.iter().find(|(_, phase)| *phase == VmPhase::Running)
    else {
        return Vec::new();
    };
    vec![ApiAddressAction::Retarget {
        address: observation.address.clone(),
        expected_generation: observation.address_generation,
        vm: successor.clone(),
    }]
}

pub struct ApiFailoverController {
    store: Arc<dyn Store>,
}

impl ApiFailoverController {
    #[must_use]
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }
}

impl Controller for ApiFailoverController {
    type Key = ObjectReference;
    type Observation = Option<ApiAddressObservation>;
    type Action = ApiAddressAction;

    const NAME: &'static str = "kubernetes-api-failover";
    const WATCHES: &'static [WatchedKind<ObjectReference>] = &[
        WatchedKind {
            kind: "KubernetesCluster",
            mapping: KeyMapping::ObjectKey(|reference| Some(reference.clone())),
        },
        WatchedKind { kind: "Vm", mapping: KeyMapping::EveryKey },
        WatchedKind { kind: "FloatingIp", mapping: KeyMapping::EveryKey },
    ];

    async fn discover_keys(&self) -> Result<Vec<ObjectReference>, ActionError> {
        let clusters =
            list::<KubernetesCluster>(self.store.as_ref(), None, Consistency::Linearizable).await?;
        Ok(clusters
            .iter()
            .filter(|cluster| cluster.status.phase == EnvironmentPhase::Ready)
            .map(amafu_domain::Object::reference)
            .collect())
    }

    async fn observe(&self, key: &ObjectReference) -> Result<Option<ApiAddressObservation>, ActionError> {
        let store = self.store.as_ref();
        let Some(cluster) = get::<KubernetesCluster>(store, key, Consistency::Linearizable).await? else {
            return Ok(None);
        };
        let ownership = ownership_of(&cluster)?;
        let address_reference = ownership.reference::<FloatingIp>(ownership.named("api")?);
        let Some(address) = get::<FloatingIp>(store, &address_reference, Consistency::Linearizable).await?
        else {
            return Ok(None);
        };
        let project = ownership.project();
        let vms = list::<Vm>(store, Some(project), Consistency::Linearizable).await?;
        let control_plane = cluster
            .status
            .nodes
            .iter()
            .filter(|node| node.member.is_control_plane())
            .map(|node| {
                let phase = vms
                    .iter()
                    .find(|vm| vm.metadata.name == node.vm)
                    .map_or(VmPhase::Failed, |vm| vm.status.phase);
                (node.vm.clone(), phase)
            })
            .collect();
        let changes =
            list::<KubernetesClusterChange>(store, Some(project), Consistency::Linearizable).await?;
        let is_changing = changes.iter().any(|change| {
            change.specification.cluster == cluster.metadata.name && !change.status.is_finished()
        });
        Ok(Some(ApiAddressObservation {
            address: address_reference,
            address_generation: address.metadata.generation,
            target: address.specification.target.map(|target| target.vm),
            control_plane,
            is_changing,
        }))
    }

    fn plan(&self, observation: &Option<ApiAddressObservation>) -> Vec<ApiAddressAction> {
        observation.as_ref().map(plan_api_address).unwrap_or_default()
    }

    async fn apply(&self, _key: &ObjectReference, action: ApiAddressAction) -> Result<(), ActionError> {
        let ApiAddressAction::Retarget { address, expected_generation, vm } = action;
        let Some(current) =
            get::<FloatingIp>(self.store.as_ref(), &address, Consistency::Linearizable).await?
        else {
            return Ok(());
        };
        tracing::info!(address = %address, %vm, "moving a Kubernetes API address to a running control-plane VM");
        let specification = FloatingIpSpecification {
            target: Some(FloatingIpTarget { guest_address: None, vm, interface: 0 }),
            ..current.specification
        };
        let change =
            Change::UpdateSpecification { expected_generation: Some(expected_generation), specification };
        apply::<FloatingIp>(self.store.as_ref(), &address, change).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(value: &str) -> ObjectName {
        ObjectName::new(value).unwrap()
    }

    fn observation(target: &str, phases: &[(&str, VmPhase)], is_changing: bool) -> ApiAddressObservation {
        ApiAddressObservation {
            address: ObjectReference::of::<FloatingIp>(None, name("platform-api")),
            address_generation: 4,
            target: Some(name(target)),
            control_plane: phases.iter().map(|(vm, phase)| (name(vm), *phase)).collect(),
            is_changing,
        }
    }

    #[test]
    fn the_api_address_moves_only_when_its_vm_stops_and_another_runs() {
        let healthy = observation(
            "platform-cp-0",
            &[("platform-cp-0", VmPhase::Running), ("platform-cp-1", VmPhase::Running)],
            false,
        );
        assert_eq!(plan_api_address(&healthy), []);
        let stopped = observation(
            "platform-cp-0",
            &[("platform-cp-0", VmPhase::Stopped), ("platform-cp-1", VmPhase::Running)],
            false,
        );
        assert_eq!(
            plan_api_address(&stopped),
            [ApiAddressAction::Retarget {
                address: stopped.address.clone(),
                expected_generation: 4,
                vm: name("platform-cp-1")
            }]
        );
        let alone = observation("platform-cp-0", &[("platform-cp-0", VmPhase::Failed)], false);
        assert_eq!(plan_api_address(&alone), []);
    }

    #[test]
    fn a_running_change_owns_the_api_address() {
        let upgrading = observation(
            "platform-cp-0",
            &[("platform-cp-0", VmPhase::Stopped), ("platform-cp-1", VmPhase::Running)],
            true,
        );
        assert_eq!(plan_api_address(&upgrading), []);
    }
}
