use std::sync::Arc;

use amafu_domain::kinds::network::{Network, NetworkPhase, NetworkStatus};
use amafu_domain::kinds::security_group::SecurityGroup;
use amafu_domain::kinds::vm::Vm;
use amafu_domain::{Change, Object, ObjectReference};
use amafu_interfaces::objects::{apply, get, list, remove};
use amafu_interfaces::{ActionError, Consistency, Store};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};

pub struct NetworkController {
    store: Arc<dyn Store>,
}

impl NetworkController {
    #[must_use]
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NetworkObservation {
    pub network: Option<Object<Network>>,
    pub others: Vec<Object<Network>>,
    pub is_in_use: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkAction {
    Report { observed_generation: u64, status: NetworkStatus },
    Remove,
}

#[must_use]
pub fn plan_network(observation: &NetworkObservation) -> Vec<NetworkAction> {
    let Some(network) = &observation.network else {
        return Vec::new();
    };
    if network.metadata.is_deleting {
        return if observation.is_in_use { Vec::new() } else { vec![NetworkAction::Remove] };
    }
    let subnet = network.specification.subnet;
    let conflict = observation.others.iter().find(|other| {
        other.metadata.id != network.metadata.id
            && !other.metadata.is_deleting
            && other.status.is_ready()
            && other.specification.subnet.overlaps(subnet)
    });
    let status = match conflict {
        Some(other) if !network.status.is_ready() => NetworkStatus {
            phase: NetworkPhase::Failed,
            gateway: None,
            message: Some(format!("{subnet} overlaps network {}", other.reference())),
        },
        _ => NetworkStatus { phase: NetworkPhase::Ready, gateway: Some(subnet.gateway()), message: None },
    };
    if network.status == status && network.is_status_current() {
        return Vec::new();
    }
    vec![NetworkAction::Report { observed_generation: network.metadata.generation, status }]
}

impl Controller for NetworkController {
    type Key = ObjectReference;
    type Observation = NetworkObservation;
    type Action = NetworkAction;

    const NAME: &'static str = "network";
    const WATCHES: &'static [WatchedKind<ObjectReference>] = &[
        WatchedKind { kind: "Network", mapping: KeyMapping::ObjectKey(|reference| Some(reference.clone())) },
        WatchedKind { kind: "Vm", mapping: KeyMapping::EveryKey },
    ];

    async fn discover_keys(&self) -> Result<Vec<ObjectReference>, ActionError> {
        let networks = list::<Network>(self.store.as_ref(), None, Consistency::Linearizable).await?;
        Ok(networks.iter().map(Object::reference).collect())
    }

    async fn observe(&self, key: &ObjectReference) -> Result<NetworkObservation, ActionError> {
        let store = self.store.as_ref();
        let network = get::<Network>(store, key, Consistency::Linearizable).await?;
        let others = list::<Network>(store, None, Consistency::Linearizable).await?;
        let is_in_use = match &network {
            Some(network) => list::<Vm>(store, network.metadata.project.as_ref(), Consistency::Linearizable)
                .await?
                .iter()
                .any(|vm| {
                    vm.specification
                        .network_interfaces
                        .iter()
                        .any(|interface| interface.network == network.metadata.name)
                }),
            None => false,
        };
        Ok(NetworkObservation { network, others, is_in_use })
    }

    fn plan(&self, observation: &NetworkObservation) -> Vec<NetworkAction> {
        plan_network(observation)
    }

    async fn apply(&self, key: &ObjectReference, action: NetworkAction) -> Result<(), ActionError> {
        match action {
            NetworkAction::Report { observed_generation, status } => {
                apply::<Network>(
                    self.store.as_ref(),
                    key,
                    Change::UpdateStatus { observed_generation, status },
                )
                .await?;
            }
            NetworkAction::Remove => remove::<Network>(self.store.as_ref(), key).await?,
        }
        Ok(())
    }
}

pub struct SecurityGroupController {
    store: Arc<dyn Store>,
}

impl SecurityGroupController {
    #[must_use]
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SecurityGroupObservation {
    pub is_deleting: bool,
    pub is_in_use: bool,
}

#[must_use]
pub fn plan_security_group(observation: &SecurityGroupObservation) -> Vec<NetworkAction> {
    if observation.is_deleting && !observation.is_in_use { vec![NetworkAction::Remove] } else { Vec::new() }
}

impl Controller for SecurityGroupController {
    type Key = ObjectReference;
    type Observation = SecurityGroupObservation;
    type Action = NetworkAction;

    const NAME: &'static str = "security-group";
    const WATCHES: &'static [WatchedKind<ObjectReference>] = &[
        WatchedKind {
            kind: "SecurityGroup",
            mapping: KeyMapping::ObjectKey(|reference| Some(reference.clone())),
        },
        WatchedKind { kind: "Vm", mapping: KeyMapping::EveryKey },
    ];

    async fn discover_keys(&self) -> Result<Vec<ObjectReference>, ActionError> {
        let groups = list::<SecurityGroup>(self.store.as_ref(), None, Consistency::Linearizable).await?;
        Ok(groups.iter().filter(|group| group.metadata.is_deleting).map(Object::reference).collect())
    }

    async fn observe(&self, key: &ObjectReference) -> Result<SecurityGroupObservation, ActionError> {
        let store = self.store.as_ref();
        let Some(group) = get::<SecurityGroup>(store, key, Consistency::Linearizable).await? else {
            return Ok(SecurityGroupObservation { is_deleting: false, is_in_use: false });
        };
        let is_in_use = list::<Vm>(store, group.metadata.project.as_ref(), Consistency::Linearizable)
            .await?
            .iter()
            .any(|vm| {
                vm.specification
                    .network_interfaces
                    .iter()
                    .any(|interface| interface.security_groups.contains(&group.metadata.name))
            });
        Ok(SecurityGroupObservation { is_deleting: group.metadata.is_deleting, is_in_use })
    }

    fn plan(&self, observation: &SecurityGroupObservation) -> Vec<NetworkAction> {
        plan_security_group(observation)
    }

    async fn apply(&self, key: &ObjectReference, action: NetworkAction) -> Result<(), ActionError> {
        if action == NetworkAction::Remove {
            remove::<SecurityGroup>(self.store.as_ref(), key).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use amafu_domain::kinds::network::NetworkSpecification;

    use super::*;
    use crate::testing::{metadata, only};

    fn network(name: &str, subnet: &str, phase: NetworkPhase) -> Object<Network> {
        Object {
            metadata: metadata(name, Some("project-a")),
            specification: NetworkSpecification {
                subnet: subnet.parse().unwrap(),
                is_outbound_access_allowed: true,
            },
            status: NetworkStatus { phase, ..NetworkStatus::default() },
        }
    }

    #[test]
    fn a_free_subnet_becomes_ready_with_its_gateway() {
        let observation = NetworkObservation {
            network: Some(network("default", "10.1.0.0/24", NetworkPhase::Pending)),
            others: vec![network("other", "10.2.0.0/24", NetworkPhase::Ready)],
            is_in_use: false,
        };
        let NetworkAction::Report { status, .. } = only(plan_network(&observation)) else {
            panic!("expected a report")
        };
        assert_eq!(status.phase, NetworkPhase::Ready);
        assert_eq!(status.gateway, Some("10.1.0.1".parse().unwrap()));
    }

    #[test]
    fn an_overlapping_subnet_fails_but_a_ready_one_keeps_its_place() {
        let existing = network("existing", "10.0.0.0/16", NetworkPhase::Ready);
        let newcomer = NetworkObservation {
            network: Some(network("newcomer", "10.0.5.0/24", NetworkPhase::Pending)),
            others: vec![existing.clone()],
            is_in_use: false,
        };
        let NetworkAction::Report { status, .. } = only(plan_network(&newcomer)) else {
            panic!("expected a report")
        };
        assert_eq!(status.phase, NetworkPhase::Failed);
        let mut ready_existing = existing.clone();
        ready_existing.status.gateway = Some("10.0.0.1".parse().unwrap());
        let keeper = NetworkObservation {
            network: Some(ready_existing),
            others: vec![network("rival", "10.0.0.0/24", NetworkPhase::Ready)],
            is_in_use: false,
        };
        assert_eq!(plan_network(&keeper), Vec::new());
    }

    #[test]
    fn a_network_in_use_is_not_removed() {
        let mut deleting = network("default", "10.1.0.0/24", NetworkPhase::Ready);
        deleting.metadata.is_deleting = true;
        let in_use =
            NetworkObservation { network: Some(deleting.clone()), others: Vec::new(), is_in_use: true };
        assert_eq!(plan_network(&in_use), Vec::new());
        let unused = NetworkObservation { is_in_use: false, ..in_use };
        assert_eq!(plan_network(&unused), vec![NetworkAction::Remove]);
        assert_eq!(
            plan_security_group(&SecurityGroupObservation { is_deleting: true, is_in_use: true }),
            Vec::new()
        );
        assert_eq!(
            plan_security_group(&SecurityGroupObservation { is_deleting: true, is_in_use: false }),
            vec![NetworkAction::Remove]
        );
    }
}
