use std::collections::BTreeSet;
use std::net::Ipv4Addr;

use amafu_domain::kinds::network::Network;
use amafu_domain::kinds::vm::{InterfaceStatus, Vm};
use amafu_domain::{MacAddress, Object};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllocationProblem {
    NotYet(String),
    Impossible(String),
}

#[must_use]
pub fn interface_mac(vm: &Object<Vm>, index: usize) -> MacAddress {
    let mut seed = vm.metadata.id.as_uuid().as_bytes().to_vec();
    seed.extend_from_slice(&index.to_be_bytes());
    MacAddress::locally_administered(&seed)
}

fn addresses_in_use(network: &Object<Network>, peers: &[Object<Vm>], vm: &Object<Vm>) -> BTreeSet<Ipv4Addr> {
    peers
        .iter()
        .filter(|peer| peer.metadata.id != vm.metadata.id)
        .flat_map(|peer| peer.status.interfaces.iter())
        .filter(|interface| interface.network == network.metadata.name)
        .map(|interface| interface.address)
        .collect()
}

pub fn allocate_interfaces(
    vm: &Object<Vm>,
    networks: &[Object<Network>],
    peers: &[Object<Vm>],
) -> Result<Vec<InterfaceStatus>, AllocationProblem> {
    let mut allocated: Vec<InterfaceStatus> = Vec::new();
    for (index, interface) in vm.specification.network_interfaces.iter().enumerate() {
        let network = networks
            .iter()
            .find(|network| network.metadata.name == interface.network && !network.metadata.is_deleting)
            .ok_or_else(|| {
                AllocationProblem::NotYet(format!("network {} does not exist", interface.network))
            })?;
        if !network.status.is_ready() {
            return Err(AllocationProblem::NotYet(format!("network {} is not ready", interface.network)));
        }
        let subnet = network.specification.subnet;
        let mut in_use = addresses_in_use(network, peers, vm);
        in_use.extend(
            allocated.iter().filter(|done| done.network == interface.network).map(|done| done.address),
        );
        let address = match interface.address {
            Some(requested) if !subnet.is_assignable(requested) => {
                return Err(AllocationProblem::Impossible(format!(
                    "{requested} cannot be assigned in {subnet}"
                )));
            }
            Some(requested) if in_use.contains(&requested) => {
                return Err(AllocationProblem::Impossible(format!("{requested} is already in use")));
            }
            Some(requested) => requested,
            None => subnet.assignable_addresses().find(|candidate| !in_use.contains(candidate)).ok_or_else(
                || AllocationProblem::NotYet(format!("network {} has no free address", interface.network)),
            )?,
        };
        allocated.push(InterfaceStatus {
            network: interface.network.clone(),
            mac: interface_mac(vm, index),
            address,
        });
    }
    Ok(allocated)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use amafu_domain::kinds::network::{NetworkPhase, NetworkSpecification, NetworkStatus};
    use amafu_domain::kinds::vm::{NetworkInterfaceSpecification, VmSpecification, VmStatus};
    use amafu_domain::{Metadata, ObjectId, ObjectName, ProjectName};

    use super::*;

    fn metadata(name: &str) -> Metadata {
        Metadata {
            name: ObjectName::new(name).unwrap(),
            project: Some(ProjectName::new("project-a").unwrap()),
            id: ObjectId::from_uuid(uuid::Uuid::new_v4()),
            version: 1,
            generation: 1,
            observed_generation: 1,
            labels: BTreeMap::new(),
            is_deleting: false,
        }
    }

    fn network(phase: NetworkPhase) -> Object<Network> {
        Object {
            metadata: metadata("default"),
            specification: NetworkSpecification {
                subnet: "10.1.0.0/29".parse().unwrap(),
                is_outbound_access_allowed: true,
            },
            status: NetworkStatus { phase, ..NetworkStatus::default() },
        }
    }

    fn vm(name: &str, address: Option<Ipv4Addr>) -> Object<Vm> {
        let interface = NetworkInterfaceSpecification {
            network: ObjectName::new("default").unwrap(),
            security_groups: Vec::new(),
            address,
        };
        Object {
            metadata: metadata(name),
            specification: VmSpecification {
                network_interfaces: vec![interface],
                ..VmSpecification::small()
            },
            status: VmStatus::default(),
        }
    }

    fn allocated(vm: &Object<Vm>, peers: &[Object<Vm>]) -> Result<Vec<Ipv4Addr>, AllocationProblem> {
        Ok(allocate_interfaces(vm, &[network(NetworkPhase::Ready)], peers)?
            .iter()
            .map(|interface| interface.address)
            .collect())
    }

    #[test]
    fn the_lowest_free_address_after_the_gateway_is_chosen() {
        let first = vm("first", None);
        assert_eq!(allocated(&first, &[]), Ok(vec![Ipv4Addr::new(10, 1, 0, 2)]));
        let mut placed = first.clone();
        placed.status.interfaces = allocate_interfaces(&first, &[network(NetworkPhase::Ready)], &[]).unwrap();
        assert_eq!(allocated(&vm("second", None), &[placed]), Ok(vec![Ipv4Addr::new(10, 1, 0, 3)]));
    }

    #[test]
    fn requested_addresses_are_checked() {
        let taken = {
            let mut peer = vm("peer", Some(Ipv4Addr::new(10, 1, 0, 4)));
            peer.status.interfaces =
                allocate_interfaces(&peer, &[network(NetworkPhase::Ready)], &[]).unwrap();
            peer
        };
        assert_eq!(
            allocated(&vm("fixed", Some(Ipv4Addr::new(10, 1, 0, 5))), std::slice::from_ref(&taken)),
            Ok(vec![Ipv4Addr::new(10, 1, 0, 5)])
        );
        assert!(matches!(
            allocated(&vm("clash", Some(Ipv4Addr::new(10, 1, 0, 4))), &[taken]),
            Err(AllocationProblem::Impossible(_))
        ));
        assert!(matches!(
            allocated(&vm("gateway", Some(Ipv4Addr::new(10, 1, 0, 1))), &[]),
            Err(AllocationProblem::Impossible(_))
        ));
        assert!(matches!(
            allocated(&vm("outside", Some(Ipv4Addr::new(10, 2, 0, 5))), &[]),
            Err(AllocationProblem::Impossible(_))
        ));
    }

    #[test]
    fn a_full_or_unready_network_waits() {
        assert!(matches!(
            allocate_interfaces(&vm("early", None), &[network(NetworkPhase::Pending)], &[]),
            Err(AllocationProblem::NotYet(_))
        ));
        let mut peers = Vec::new();
        for index in 0..5 {
            let mut peer = vm(&format!("peer-{index}"), None);
            peer.status.interfaces =
                allocate_interfaces(&peer, &[network(NetworkPhase::Ready)], &peers).unwrap();
            peers.push(peer);
        }
        assert!(matches!(allocated(&vm("late", None), &peers), Err(AllocationProblem::NotYet(_))));
    }

    #[test]
    fn mac_addresses_are_stable_and_distinct_per_interface() {
        let machine = vm("web", None);
        assert_eq!(interface_mac(&machine, 0), interface_mac(&machine, 0));
        assert_ne!(interface_mac(&machine, 0), interface_mac(&machine, 1));
    }
}
