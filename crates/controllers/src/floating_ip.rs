use std::collections::BTreeSet;
use std::net::Ipv4Addr;
use std::sync::Arc;

use amafu_domain::kinds::external_network::ExternalNetwork;
use amafu_domain::kinds::floating_ip::{FloatingIp, FloatingIpBinding, FloatingIpPhase, FloatingIpStatus};
use amafu_domain::kinds::vm::Vm;
use amafu_domain::{Change, Object, ObjectReference};
use amafu_interfaces::objects::{apply, get, list, remove};
use amafu_interfaces::{ActionError, Consistency, Store};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};

use crate::references::{cluster_reference, project_reference};

pub struct FloatingIpController {
    store: Arc<dyn Store>,
}

impl FloatingIpController {
    #[must_use]
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FloatingIpObservation {
    pub floating_ip: Option<Object<FloatingIp>>,
    pub external_network: Option<Object<ExternalNetwork>>,
    pub taken: BTreeSet<Ipv4Addr>,
    pub target: Option<Object<Vm>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FloatingIpAction {
    Report { observed_generation: u64, status: FloatingIpStatus },
    Remove,
}

fn failed(reason: String) -> FloatingIpStatus {
    FloatingIpStatus { phase: FloatingIpPhase::Failed, message: Some(reason), ..FloatingIpStatus::default() }
}

fn allocate(
    floating_ip: &Object<FloatingIp>,
    observation: &FloatingIpObservation,
) -> Result<Ipv4Addr, String> {
    let Some(external) = &observation.external_network else {
        return Err(format!(
            "external network {} does not exist",
            floating_ip.specification.external_network
        ));
    };
    if let Some(address) = floating_ip.status.address {
        return Ok(address);
    }
    let range = &external.specification;
    match floating_ip.specification.address {
        Some(requested) if !range.contains(requested) => {
            Err(format!("{requested} is outside the external network's range"))
        }
        Some(requested) if observation.taken.contains(&requested) => {
            Err(format!("{requested} is already in use"))
        }
        Some(requested) => Ok(requested),
        None => range.addresses().find(|candidate| !observation.taken.contains(candidate)).ok_or_else(|| {
            format!("external network {} has no free address", floating_ip.specification.external_network)
        }),
    }
}

fn binding(floating_ip: &Object<FloatingIp>, target: Option<&Object<Vm>>) -> Option<FloatingIpBinding> {
    let wanted = floating_ip.specification.target.as_ref()?;
    let vm = target.filter(|vm| vm.metadata.name == wanted.vm && !vm.metadata.is_deleting)?;
    let node = vm.status.node.clone()?;
    let interface = vm.status.interfaces.get(usize::from(wanted.interface))?;
    Some(FloatingIpBinding { node, vm_id: vm.metadata.id, private_address: interface.address })
}

#[must_use]
pub fn plan_floating_ip(observation: &FloatingIpObservation) -> Vec<FloatingIpAction> {
    let Some(floating_ip) = &observation.floating_ip else {
        return Vec::new();
    };
    if floating_ip.metadata.is_deleting {
        return vec![FloatingIpAction::Remove];
    }
    let status = match allocate(floating_ip, observation) {
        Err(reason) => failed(reason),
        Ok(address) => {
            let binding = binding(floating_ip, observation.target.as_ref());
            let phase = if binding.is_some() { FloatingIpPhase::Bound } else { FloatingIpPhase::Allocated };
            let message = match (&binding, &floating_ip.specification.target) {
                (None, Some(target)) => Some(format!("waiting for VM {} to be placed", target.vm)),
                _ => None,
            };
            FloatingIpStatus { phase, address: Some(address), binding, message }
        }
    };
    if floating_ip.status == status && floating_ip.is_status_current() {
        return Vec::new();
    }
    vec![FloatingIpAction::Report { observed_generation: floating_ip.metadata.generation, status }]
}

impl Controller for FloatingIpController {
    type Key = ObjectReference;
    type Observation = FloatingIpObservation;
    type Action = FloatingIpAction;

    const NAME: &'static str = "floating-ip";
    const WATCHES: &'static [WatchedKind<ObjectReference>] = &[
        WatchedKind {
            kind: "FloatingIp",
            mapping: KeyMapping::ObjectKey(|reference| Some(reference.clone())),
        },
        WatchedKind { kind: "Vm", mapping: KeyMapping::EveryKey },
        WatchedKind { kind: "ExternalNetwork", mapping: KeyMapping::EveryKey },
    ];

    async fn discover_keys(&self) -> Result<Vec<ObjectReference>, ActionError> {
        let floating_ips = list::<FloatingIp>(self.store.as_ref(), None, Consistency::Linearizable).await?;
        Ok(floating_ips.iter().map(Object::reference).collect())
    }

    async fn observe(&self, key: &ObjectReference) -> Result<FloatingIpObservation, ActionError> {
        let store = self.store.as_ref();
        let floating_ip = get::<FloatingIp>(store, key, Consistency::Linearizable).await?;
        let mut observation = FloatingIpObservation {
            floating_ip: None,
            external_network: None,
            taken: BTreeSet::new(),
            target: None,
        };
        let Some(floating_ip) = floating_ip else {
            return Ok(observation);
        };
        let network_name = &floating_ip.specification.external_network;
        observation.external_network = get::<ExternalNetwork>(
            store,
            &cluster_reference::<ExternalNetwork>(network_name),
            Consistency::Linearizable,
        )
        .await?;
        observation.taken = list::<FloatingIp>(store, None, Consistency::Linearizable)
            .await?
            .iter()
            .filter(|other| {
                other.metadata.id != floating_ip.metadata.id
                    && other.specification.external_network == *network_name
            })
            .filter_map(|other| other.status.address)
            .collect();
        if let Some(target) = &floating_ip.specification.target {
            observation.target = get::<Vm>(
                store,
                &project_reference::<Vm>(&floating_ip, &target.vm),
                Consistency::Linearizable,
            )
            .await?;
        }
        observation.floating_ip = Some(floating_ip);
        Ok(observation)
    }

    fn plan(&self, observation: &FloatingIpObservation) -> Vec<FloatingIpAction> {
        plan_floating_ip(observation)
    }

    async fn apply(&self, key: &ObjectReference, action: FloatingIpAction) -> Result<(), ActionError> {
        let store = self.store.as_ref();
        match action {
            FloatingIpAction::Report { observed_generation, status } => {
                apply::<FloatingIp>(store, key, Change::UpdateStatus { observed_generation, status }).await?;
            }
            FloatingIpAction::Remove => remove::<FloatingIp>(store, key).await?,
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use amafu_domain::MacAddress;
    use amafu_domain::NodeName;
    use amafu_domain::kinds::external_network::{ExternalNetworkSpecification, ExternalNetworkStatus};
    use amafu_domain::kinds::floating_ip::{FloatingIpSpecification, FloatingIpTarget};
    use amafu_domain::kinds::vm::{InterfaceStatus, VmSpecification, VmStatus};

    use super::*;
    use crate::testing::{metadata, name};

    fn external() -> Object<ExternalNetwork> {
        Object {
            metadata: metadata("lab", None),
            specification: ExternalNetworkSpecification {
                subnet: "198.51.100.0/24".parse().unwrap(),
                first_address: Ipv4Addr::new(198, 51, 100, 10),
                last_address: Ipv4Addr::new(198, 51, 100, 11),
            },
            status: ExternalNetworkStatus {},
        }
    }

    fn floating(target: Option<&str>) -> Object<FloatingIp> {
        Object {
            metadata: metadata("public", Some("project-a")),
            specification: FloatingIpSpecification {
                external_network: name("lab"),
                address: None,
                target: target.map(|vm| FloatingIpTarget { vm: name(vm), interface: 0 }),
            },
            status: FloatingIpStatus::default(),
        }
    }

    fn placed_vm(on: &str) -> Object<Vm> {
        Object {
            metadata: metadata("web", Some("project-a")),
            specification: VmSpecification::small(),
            status: VmStatus {
                node: Some(NodeName::new(on).unwrap()),
                interfaces: vec![InterfaceStatus {
                    network: name("default"),
                    mac: MacAddress::locally_administered(b"web"),
                    address: Ipv4Addr::new(10, 1, 0, 2),
                }],
                ..VmStatus::default()
            },
        }
    }

    fn reported(observation: &FloatingIpObservation) -> FloatingIpStatus {
        match plan_floating_ip(observation).as_slice() {
            [FloatingIpAction::Report { status, .. }] => status.clone(),
            other => panic!("expected one report, got {other:?}"),
        }
    }

    #[test]
    fn the_lowest_free_address_is_allocated_and_bound_to_the_target_node() {
        let observation = FloatingIpObservation {
            floating_ip: Some(floating(Some("web"))),
            external_network: Some(external()),
            taken: BTreeSet::from([Ipv4Addr::new(198, 51, 100, 10)]),
            target: Some(placed_vm("node-2")),
        };
        let status = reported(&observation);
        assert_eq!(status.address, Some(Ipv4Addr::new(198, 51, 100, 11)));
        assert_eq!(status.phase, FloatingIpPhase::Bound);
        assert_eq!(status.binding.unwrap().node.as_str(), "node-2");
    }

    #[test]
    fn an_exhausted_range_or_missing_network_fails() {
        let exhausted = FloatingIpObservation {
            floating_ip: Some(floating(None)),
            external_network: Some(external()),
            taken: external().specification.addresses().collect(),
            target: None,
        };
        assert_eq!(reported(&exhausted).phase, FloatingIpPhase::Failed);
        let missing = FloatingIpObservation { external_network: None, taken: BTreeSet::new(), ..exhausted };
        assert_eq!(reported(&missing).phase, FloatingIpPhase::Failed);
    }

    #[test]
    fn moving_the_target_moves_the_binding_but_keeps_the_address() {
        let mut moved = floating(Some("web"));
        moved.status.address = Some(Ipv4Addr::new(198, 51, 100, 10));
        let observation = FloatingIpObservation {
            floating_ip: Some(moved),
            external_network: Some(external()),
            taken: BTreeSet::new(),
            target: Some(placed_vm("node-3")),
        };
        let status = reported(&observation);
        assert_eq!(status.address, Some(Ipv4Addr::new(198, 51, 100, 10)));
        assert_eq!(status.binding.unwrap().node.as_str(), "node-3");
    }

    #[test]
    fn an_unplaced_target_leaves_the_address_allocated() {
        let observation = FloatingIpObservation {
            floating_ip: Some(floating(Some("web"))),
            external_network: Some(external()),
            taken: BTreeSet::new(),
            target: None,
        };
        let status = reported(&observation);
        assert_eq!(status.phase, FloatingIpPhase::Allocated);
        assert!(status.message.unwrap().contains("waiting for VM web"));
    }
}
