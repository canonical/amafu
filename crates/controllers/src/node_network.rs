use std::sync::{Arc, Mutex, PoisonError};

use amafu_domain::kinds::network::Network;
use amafu_domain::kinds::security_group::{Remote, SecurityGroup};
use amafu_domain::kinds::vm::Vm;
use amafu_domain::{Ipv4Network, NodeName, Object, ObjectName};
use amafu_interfaces::network::{PortRule, RemoteAddresses};
use amafu_interfaces::objects::list;
use amafu_interfaces::{
    ActionError, Consistency, Datapath, LocalServices, NodeNetworkState, Port, PortKey, RealizedNetwork,
    Segment, SegmentKey, Store,
};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};

pub struct NodeNetworkController {
    node: NodeName,
    store: Arc<dyn Store>,
    datapath: Arc<dyn Datapath>,
    local_services: Arc<dyn LocalServices>,
    last_applied: Mutex<Option<NodeNetworkState>>,
}

impl NodeNetworkController {
    #[must_use]
    pub fn new(
        node: NodeName,
        store: Arc<dyn Store>,
        datapath: Arc<dyn Datapath>,
        local_services: Arc<dyn LocalServices>,
    ) -> Self {
        Self { node, store, datapath, local_services, last_applied: Mutex::new(None) }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodeNetworkObservation {
    pub desired: NodeNetworkState,
    pub last_applied: Option<NodeNetworkState>,
    pub realized: RealizedNetwork,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeNetworkAction {
    Converge(NodeNetworkState),
}

fn segment_key(vm: &Object<Vm>, network: &ObjectName) -> SegmentKey {
    let project = vm.metadata.project.as_ref().map(ToString::to_string).unwrap_or_default();
    SegmentKey::of(&project, network.as_str())
}

fn group_member_addresses(
    vms: &[Object<Vm>],
    project_of: &Object<Vm>,
    group: &ObjectName,
) -> Vec<Ipv4Network> {
    vms.iter()
        .filter(|vm| vm.metadata.project == project_of.metadata.project)
        .flat_map(|vm| {
            vm.specification
                .network_interfaces
                .iter()
                .zip(&vm.status.interfaces)
                .filter(|(interface, _)| interface.security_groups.contains(group))
                .map(|(_, allocated)| Ipv4Network::host(allocated.address))
        })
        .collect()
}

fn port_rules(
    vm: &Object<Vm>,
    group_names: &[ObjectName],
    groups: &[Object<SecurityGroup>],
    vms: &[Object<Vm>],
) -> Vec<PortRule> {
    let mut rules: Vec<PortRule> = group_names
        .iter()
        .filter_map(|name| {
            groups
                .iter()
                .find(|group| &group.metadata.name == name && group.metadata.project == vm.metadata.project)
        })
        .flat_map(|group| group.specification.rules.iter())
        .map(|rule| PortRule {
            direction: rule.direction,
            protocol: rule.protocol,
            ports: rule.ports,
            remote: match &rule.remote {
                Remote::Any => RemoteAddresses::Any,
                Remote::Addresses { network } => RemoteAddresses::Networks { networks: vec![*network] },
                Remote::SecurityGroup { name } => {
                    RemoteAddresses::Networks { networks: group_member_addresses(vms, vm, name) }
                }
            },
        })
        .collect();
    rules.sort();
    rules.dedup();
    rules
}

#[must_use]
pub fn desired_network_state(
    node: &NodeName,
    vms: &[Object<Vm>],
    networks: &[Object<Network>],
    groups: &[Object<SecurityGroup>],
) -> NodeNetworkState {
    let mut state = NodeNetworkState::default();
    let hosted = vms.iter().filter(|vm| vm.status.node.as_ref() == Some(node));
    for vm in hosted {
        for (index, (interface, allocated)) in
            vm.specification.network_interfaces.iter().zip(&vm.status.interfaces).enumerate()
        {
            let Some(network) = networks.iter().find(|network| {
                network.metadata.name == interface.network && network.metadata.project == vm.metadata.project
            }) else {
                continue;
            };
            let key = segment_key(vm, &interface.network);
            if state.segment(&key).is_none() {
                let project = vm.metadata.project.as_ref().map(ToString::to_string).unwrap_or_default();
                state.segments.push(Segment {
                    key: key.clone(),
                    subnet: network.specification.subnet,
                    is_outbound_access_allowed: network.specification.is_outbound_access_allowed,
                    domain: format!("{}.{project}.internal", interface.network),
                });
            }
            state.ports.push(Port {
                key: PortKey { vm_id: vm.metadata.id, index: u8::try_from(index).unwrap_or(u8::MAX) },
                segment: key,
                mac: allocated.mac,
                address: allocated.address,
                hostname: vm.metadata.name.to_string(),
                rules: port_rules(vm, &interface.security_groups, groups, vms),
                user_data: vm.specification.user_data.clone(),
            });
        }
    }
    state.segments.sort_by(|left, right| left.key.cmp(&right.key));
    state.ports.sort_by_key(|port| port.key);
    state
}

#[must_use]
pub fn plan_node_network(observation: &NodeNetworkObservation) -> Vec<NodeNetworkAction> {
    let desired = &observation.desired;
    let is_realized =
        desired.segments.iter().all(|segment| observation.realized.segments.contains(&segment.key))
            && desired.ports.iter().all(|port| observation.realized.ports.contains(&port.key))
            && observation.realized.ports.len() == desired.ports.len();
    if observation.last_applied.as_ref() == Some(desired) && is_realized {
        return Vec::new();
    }
    vec![NodeNetworkAction::Converge(desired.clone())]
}

impl Controller for NodeNetworkController {
    type Key = NodeName;
    type Observation = NodeNetworkObservation;
    type Action = NodeNetworkAction;

    const NAME: &'static str = "node-network";
    const WATCHES: &'static [WatchedKind<NodeName>] = &[
        WatchedKind { kind: "Vm", mapping: KeyMapping::EveryKey },
        WatchedKind { kind: "Network", mapping: KeyMapping::EveryKey },
        WatchedKind { kind: "SecurityGroup", mapping: KeyMapping::EveryKey },
    ];

    async fn discover_keys(&self) -> Result<Vec<NodeName>, ActionError> {
        Ok(vec![self.node.clone()])
    }

    async fn observe(&self, _key: &NodeName) -> Result<NodeNetworkObservation, ActionError> {
        let store = self.store.as_ref();
        let vms = list::<Vm>(store, None, Consistency::Linearizable).await?;
        let networks = list::<Network>(store, None, Consistency::Linearizable).await?;
        let groups = list::<SecurityGroup>(store, None, Consistency::Linearizable).await?;
        let realized = self.datapath.current().await?;
        let last_applied = self.last_applied.lock().unwrap_or_else(PoisonError::into_inner).clone();
        Ok(NodeNetworkObservation {
            desired: desired_network_state(&self.node, &vms, &networks, &groups),
            last_applied,
            realized,
        })
    }

    fn plan(&self, observation: &NodeNetworkObservation) -> Vec<NodeNetworkAction> {
        plan_node_network(observation)
    }

    async fn apply(&self, _key: &NodeName, action: NodeNetworkAction) -> Result<(), ActionError> {
        let NodeNetworkAction::Converge(desired) = action;
        self.datapath.converge(&desired).await?;
        self.local_services.update(&desired);
        *self.last_applied.lock().unwrap_or_else(PoisonError::into_inner) = Some(desired);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use amafu_domain::kinds::network::{NetworkPhase, NetworkSpecification, NetworkStatus};
    use amafu_domain::kinds::security_group::{
        Direction, PortRange, Protocol, SecurityGroupRule, SecurityGroupSpecification,
    };
    use amafu_domain::kinds::vm::{
        InterfaceStatus, NetworkInterfaceSpecification, VmSpecification, VmStatus,
    };
    use amafu_domain::{MacAddress, ProjectName};

    use super::*;
    use crate::testing::{metadata, name};

    fn node(value: &str) -> NodeName {
        NodeName::new(value).unwrap()
    }

    fn network() -> Object<Network> {
        Object {
            metadata: metadata("default", Some("project-a")),
            specification: NetworkSpecification {
                subnet: "10.1.0.0/24".parse().unwrap(),
                is_outbound_access_allowed: true,
            },
            status: NetworkStatus { phase: NetworkPhase::Ready, ..NetworkStatus::default() },
        }
    }

    fn vm(vm_name: &str, on: &str, address: [u8; 4], groups: &[&str]) -> Object<Vm> {
        let interface = NetworkInterfaceSpecification {
            network: name("default"),
            security_groups: groups.iter().map(|group| name(group)).collect(),
            address: None,
        };
        Object {
            metadata: metadata(vm_name, Some("project-a")),
            specification: VmSpecification {
                network_interfaces: vec![interface],
                ..VmSpecification::small()
            },
            status: VmStatus {
                node: Some(node(on)),
                interfaces: vec![InterfaceStatus {
                    network: name("default"),
                    mac: MacAddress::locally_administered(vm_name.as_bytes()),
                    address: Ipv4Addr::from(address),
                }],
                ..VmStatus::default()
            },
        }
    }

    fn web_group() -> Object<SecurityGroup> {
        Object {
            metadata: metadata("web", Some("project-a")),
            specification: SecurityGroupSpecification {
                rules: vec![SecurityGroupRule {
                    direction: Direction::Ingress,
                    protocol: Protocol::Tcp,
                    ports: Some(PortRange { first: 80, last: 80 }),
                    remote: Remote::SecurityGroup { name: name("clients") },
                }],
            },
            status: amafu_domain::kinds::security_group::SecurityGroupStatus {},
        }
    }

    #[test]
    fn only_ports_of_vms_on_this_node_are_desired() {
        let vms =
            vec![vm("web", "node-0", [10, 1, 0, 2], &[]), vm("elsewhere", "node-1", [10, 1, 0, 3], &[])];
        let state = desired_network_state(&node("node-0"), &vms, &[network()], &[]);
        assert_eq!(state.ports.len(), 1);
        assert_eq!(state.ports[0].hostname, "web");
        assert_eq!(state.segments.len(), 1);
        assert_eq!(state.segments[0].domain, "default.project-a.internal");
        assert_eq!(state.segments[0].gateway(), Ipv4Addr::new(10, 1, 0, 1));
    }

    #[test]
    fn security_group_remotes_become_member_addresses_cluster_wide() {
        let vms = vec![
            vm("web", "node-0", [10, 1, 0, 2], &["web"]),
            vm("client", "node-1", [10, 1, 0, 3], &["clients"]),
        ];
        let state = desired_network_state(&node("node-0"), &vms, &[network()], &[web_group()]);
        assert_eq!(
            state.ports[0].rules[0].remote,
            RemoteAddresses::Networks { networks: vec![Ipv4Network::host(Ipv4Addr::new(10, 1, 0, 3))] }
        );
    }

    #[test]
    fn groups_of_other_projects_are_ignored() {
        let mut foreign = web_group();
        foreign.metadata.project = Some(ProjectName::new("project-b").unwrap());
        let state = desired_network_state(
            &node("node-0"),
            &[vm("web", "node-0", [10, 1, 0, 2], &["web"])],
            &[network()],
            &[foreign],
        );
        assert_eq!(state.ports[0].rules, Vec::new());
    }

    #[test]
    fn the_datapath_converges_once_until_something_changes() {
        let state = desired_network_state(
            &node("node-0"),
            &[vm("web", "node-0", [10, 1, 0, 2], &[])],
            &[network()],
            &[],
        );
        let fresh = NodeNetworkObservation {
            desired: state.clone(),
            last_applied: None,
            realized: RealizedNetwork::default(),
        };
        assert_eq!(plan_node_network(&fresh), vec![NodeNetworkAction::Converge(state.clone())]);
        let realized = RealizedNetwork {
            segments: state.segments.iter().map(|segment| segment.key.clone()).collect(),
            ports: state.ports.iter().map(|port| port.key).collect(),
        };
        let settled = NodeNetworkObservation {
            desired: state.clone(),
            last_applied: Some(state.clone()),
            realized: realized.clone(),
        };
        assert_eq!(plan_node_network(&settled), Vec::new());
        let lost = NodeNetworkObservation { realized: RealizedNetwork::default(), ..settled };
        assert_eq!(plan_node_network(&lost).len(), 1);
    }
}
