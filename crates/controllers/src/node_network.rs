use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex, PoisonError};

use amafu_domain::kinds::floating_ip::FloatingIp;
use amafu_domain::kinds::network::Network;
use amafu_domain::kinds::network_endpoint::{NetworkEndpoint, NetworkEndpointSpecification};
use amafu_domain::kinds::security_group::{Remote, SecurityGroup};
use amafu_domain::kinds::vm::Vm;
use amafu_domain::{
    Change, Ipv4Network, NodeName, Object, ObjectId, ObjectName, ObjectReference, ProjectName, Resource,
};
use amafu_interfaces::network::{PortRule, RemoteAddresses};
use amafu_interfaces::objects::{apply, list, remove};
use amafu_interfaces::{
    ActionError, Consistency, Datapath, FloatingAddress, LocalServices, NodeNetworkState, Port, PortKey,
    RealizedNetwork, RemoteEndpoint, Segment, SegmentKey, Store,
};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};

pub struct NodeNetworkController {
    node: NodeName,
    underlay_address: Option<Ipv4Addr>,
    store: Arc<dyn Store>,
    datapath: Arc<dyn Datapath>,
    local_services: Arc<dyn LocalServices>,
    last_applied: Mutex<Option<NodeNetworkState>>,
}

impl NodeNetworkController {
    #[must_use]
    pub fn new(
        node: NodeName,
        underlay_address: Option<Ipv4Addr>,
        store: Arc<dyn Store>,
        datapath: Arc<dyn Datapath>,
        local_services: Arc<dyn LocalServices>,
    ) -> Self {
        Self { node, underlay_address, store, datapath, local_services, last_applied: Mutex::new(None) }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NetworkView {
    pub node: NodeName,
    pub underlay_address: Option<Ipv4Addr>,
    pub vms: Vec<Object<Vm>>,
    pub networks: Vec<Object<Network>>,
    pub groups: Vec<Object<SecurityGroup>>,
    pub endpoints: Vec<Object<NetworkEndpoint>>,
    pub floating_ips: Vec<Object<FloatingIp>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodeNetworkObservation {
    pub desired: NodeNetworkState,
    pub last_applied: Option<NodeNetworkState>,
    pub realized: RealizedNetwork,
    pub wanted_endpoints: BTreeMap<ObjectReference, NetworkEndpointSpecification>,
    pub published_endpoints: BTreeMap<ObjectReference, NetworkEndpointSpecification>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeNetworkAction {
    Converge(NodeNetworkState),
    Publish { endpoint: ObjectReference, specification: NetworkEndpointSpecification },
    Withdraw { endpoint: ObjectReference },
}

fn project_key(project: Option<&ProjectName>) -> String {
    project.map(ToString::to_string).unwrap_or_default()
}

fn segment_key(project: Option<&ProjectName>, network: &ObjectName) -> SegmentKey {
    SegmentKey::of(&project_key(project), network.as_str())
}

fn hosted(view: &NetworkView) -> impl Iterator<Item = &Object<Vm>> {
    view.vms.iter().filter(|vm| vm.status.node.as_ref() == Some(&view.node))
}

fn group_member_addresses(
    view: &NetworkView,
    project: Option<&ProjectName>,
    group: &ObjectName,
) -> Vec<Ipv4Network> {
    let from_endpoints = view
        .endpoints
        .iter()
        .filter(|endpoint| endpoint.metadata.project.as_ref() == project)
        .filter(|endpoint| endpoint.specification.security_groups.contains(group))
        .map(|endpoint| endpoint.specification.address);
    let from_hosted = hosted(view).filter(|vm| vm.metadata.project.as_ref() == project).flat_map(|vm| {
        vm.specification
            .network_interfaces
            .iter()
            .zip(&vm.status.interfaces)
            .filter(|(interface, _)| interface.security_groups.contains(group))
            .map(|(_, allocated)| allocated.address)
    });
    let mut addresses: Vec<Ipv4Network> = from_endpoints.chain(from_hosted).map(Ipv4Network::host).collect();
    addresses.sort();
    addresses.dedup();
    addresses
}

fn port_rules(view: &NetworkView, vm: &Object<Vm>, group_names: &[ObjectName]) -> Vec<PortRule> {
    let project = vm.metadata.project.as_ref();
    let mut rules: Vec<PortRule> = group_names
        .iter()
        .filter_map(|name| {
            view.groups
                .iter()
                .find(|group| &group.metadata.name == name && group.metadata.project.as_ref() == project)
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
                    RemoteAddresses::Networks { networks: group_member_addresses(view, project, name) }
                }
            },
        })
        .collect();
    rules.sort();
    rules.dedup();
    rules
}

fn port_index(index: usize) -> u8 {
    u8::try_from(index).unwrap_or(u8::MAX)
}

fn add_segment(state: &mut NodeNetworkState, network: &Object<Network>) -> SegmentKey {
    let project = network.metadata.project.as_ref();
    let key = segment_key(project, &network.metadata.name);
    if state.segment(&key).is_none() {
        state.segments.push(Segment {
            key: key.clone(),
            subnet: network.specification.subnet,
            is_outbound_access_allowed: network.specification.is_outbound_access_allowed,
            domain: format!("{}.{}.internal", network.metadata.name, project_key(project)),
            segment_id: network.status.segment_id,
        });
    }
    key
}

#[must_use]
pub fn desired_network_state(view: &NetworkView) -> NodeNetworkState {
    let mut state =
        NodeNetworkState { underlay_address: view.underlay_address, ..NodeNetworkState::default() };
    for vm in hosted(view) {
        let project = vm.metadata.project.as_ref();
        for (index, (interface, allocated)) in
            vm.specification.network_interfaces.iter().zip(&vm.status.interfaces).enumerate()
        {
            let Some(network) = view.networks.iter().find(|network| {
                network.metadata.name == interface.network && network.metadata.project.as_ref() == project
            }) else {
                continue;
            };
            let key = add_segment(&mut state, network);
            state.ports.push(Port {
                key: PortKey { vm_id: vm.metadata.id, index: port_index(index) },
                segment: key,
                mac: allocated.mac,
                address: allocated.address,
                hostname: vm.metadata.name.to_string(),
                rules: port_rules(view, vm, &interface.security_groups),
                user_data: vm.specification.user_data.clone(),
            });
        }
    }
    state.remote_endpoints = view
        .endpoints
        .iter()
        .filter(|endpoint| endpoint.specification.node != view.node)
        .filter_map(|endpoint| {
            let segment = segment_key(endpoint.metadata.project.as_ref(), &endpoint.specification.network);
            state.segment(&segment)?;
            Some(RemoteEndpoint {
                segment,
                mac: endpoint.specification.mac,
                address: endpoint.specification.address,
                hostname: endpoint.specification.vm.to_string(),
                node_address: endpoint.specification.node_address,
            })
        })
        .collect();
    state.floating_addresses = view
        .floating_ips
        .iter()
        .filter_map(|floating_ip| {
            let binding = floating_ip.status.binding.as_ref().filter(|binding| binding.node == view.node)?;
            let port = state
                .ports
                .iter()
                .find(|port| port.key.vm_id == binding.vm_id && port.address == binding.private_address)?;
            Some(FloatingAddress {
                address: floating_ip.status.address?,
                private_address: binding.private_address,
                segment: port.segment.clone(),
            })
        })
        .collect();
    state.segments.sort_by(|left, right| left.key.cmp(&right.key));
    state.ports.sort_by_key(|port| port.key);
    state.remote_endpoints.sort();
    state.floating_addresses.sort();
    state
}

#[must_use]
pub fn wanted_endpoints(view: &NetworkView) -> BTreeMap<ObjectReference, NetworkEndpointSpecification> {
    let Some(node_address) = view.underlay_address else {
        return BTreeMap::new();
    };
    let mut wanted = BTreeMap::new();
    for vm in hosted(view) {
        for (index, (interface, allocated)) in
            vm.specification.network_interfaces.iter().zip(&vm.status.interfaces).enumerate()
        {
            let index = port_index(index);
            let reference = ObjectReference::of::<NetworkEndpoint>(
                vm.metadata.project.clone(),
                NetworkEndpoint::name_for(vm.metadata.id, index),
            );
            wanted.insert(
                reference,
                NetworkEndpointSpecification {
                    network: interface.network.clone(),
                    vm: vm.metadata.name.clone(),
                    vm_id: vm.metadata.id,
                    index,
                    mac: allocated.mac,
                    address: allocated.address,
                    node: view.node.clone(),
                    node_address,
                    security_groups: interface.security_groups.clone(),
                },
            );
        }
    }
    wanted
}

#[must_use]
pub fn published_endpoints(view: &NetworkView) -> BTreeMap<ObjectReference, NetworkEndpointSpecification> {
    view.endpoints
        .iter()
        .filter(|endpoint| endpoint.specification.node == view.node)
        .map(|endpoint| (endpoint.reference(), endpoint.specification.clone()))
        .collect()
}

#[must_use]
pub fn plan_node_network(observation: &NodeNetworkObservation) -> Vec<NodeNetworkAction> {
    let mut actions = Vec::new();
    let desired = &observation.desired;
    let is_realized =
        desired.segments.iter().all(|segment| observation.realized.segments.contains(&segment.key))
            && desired.ports.iter().all(|port| observation.realized.ports.contains(&port.key))
            && observation.realized.ports.len() == desired.ports.len();
    if observation.last_applied.as_ref() != Some(desired) || !is_realized {
        actions.push(NodeNetworkAction::Converge(desired.clone()));
    }
    for (endpoint, specification) in &observation.wanted_endpoints {
        let port = PortKey { vm_id: specification.vm_id, index: specification.index };
        if observation.realized.ports.contains(&port)
            && observation.published_endpoints.get(endpoint) != Some(specification)
        {
            actions.push(NodeNetworkAction::Publish {
                endpoint: endpoint.clone(),
                specification: specification.clone(),
            });
        }
    }
    for endpoint in observation.published_endpoints.keys() {
        if !observation.wanted_endpoints.contains_key(endpoint) {
            actions.push(NodeNetworkAction::Withdraw { endpoint: endpoint.clone() });
        }
    }
    actions
}

fn endpoint_id(specification: &NetworkEndpointSpecification) -> ObjectId {
    let mut bytes = *specification.vm_id.as_uuid().as_bytes();
    bytes[15] ^= specification.index.wrapping_add(1);
    bytes[14] ^= 0x5e;
    ObjectId::from_uuid(uuid::Uuid::from_bytes(bytes))
}

impl NodeNetworkController {
    async fn list_preferring_fresh<R: Resource>(&self) -> Result<Vec<Object<R>>, ActionError> {
        match list::<R>(self.store.as_ref(), None, Consistency::Linearizable).await {
            Ok(objects) => Ok(objects),
            Err(error) => {
                let error = ActionError::from(error);
                if !error.is_retryable() {
                    return Err(error);
                }
                Ok(list::<R>(self.store.as_ref(), None, Consistency::Stale).await?)
            }
        }
    }

    async fn view(&self) -> Result<NetworkView, ActionError> {
        Ok(NetworkView {
            node: self.node.clone(),
            underlay_address: self.underlay_address,
            vms: self.list_preferring_fresh::<Vm>().await?,
            networks: self.list_preferring_fresh::<Network>().await?,
            groups: self.list_preferring_fresh::<SecurityGroup>().await?,
            endpoints: self.list_preferring_fresh::<NetworkEndpoint>().await?,
            floating_ips: self.list_preferring_fresh::<FloatingIp>().await?,
        })
    }
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
        WatchedKind { kind: "NetworkEndpoint", mapping: KeyMapping::EveryKey },
        WatchedKind { kind: "FloatingIp", mapping: KeyMapping::EveryKey },
    ];

    async fn discover_keys(&self) -> Result<Vec<NodeName>, ActionError> {
        Ok(vec![self.node.clone()])
    }

    async fn observe(&self, _key: &NodeName) -> Result<NodeNetworkObservation, ActionError> {
        let view = self.view().await?;
        let realized = self.datapath.current().await?;
        let last_applied = self.last_applied.lock().unwrap_or_else(PoisonError::into_inner).clone();
        Ok(NodeNetworkObservation {
            desired: desired_network_state(&view),
            last_applied,
            realized,
            wanted_endpoints: wanted_endpoints(&view),
            published_endpoints: published_endpoints(&view),
        })
    }

    fn plan(&self, observation: &NodeNetworkObservation) -> Vec<NodeNetworkAction> {
        plan_node_network(observation)
    }

    async fn apply(&self, _key: &NodeName, action: NodeNetworkAction) -> Result<(), ActionError> {
        let store = self.store.as_ref();
        match action {
            NodeNetworkAction::Converge(desired) => {
                self.datapath.converge(&desired).await?;
                self.local_services.update(&desired);
                *self.last_applied.lock().unwrap_or_else(PoisonError::into_inner) = Some(desired);
            }
            NodeNetworkAction::Publish { endpoint, specification } => {
                let exists = amafu_interfaces::objects::get::<NetworkEndpoint>(
                    store,
                    &endpoint,
                    Consistency::Linearizable,
                )
                .await?
                .is_some();
                let change = if exists {
                    Change::UpdateSpecification { expected_generation: None, specification }
                } else {
                    Change::Create {
                        id: endpoint_id(&specification),
                        name: endpoint.name.clone(),
                        project: endpoint.project.clone(),
                        labels: BTreeMap::new(),
                        specification,
                    }
                };
                apply::<NetworkEndpoint>(store, &endpoint, change).await?;
            }
            NodeNetworkAction::Withdraw { endpoint } => {
                apply::<NetworkEndpoint>(store, &endpoint, Change::Delete).await?;
                remove::<NetworkEndpoint>(store, &endpoint).await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use amafu_domain::kinds::floating_ip::{
        FloatingIpBinding, FloatingIpPhase, FloatingIpSpecification, FloatingIpStatus,
    };
    use amafu_domain::kinds::network::{NetworkPhase, NetworkSpecification, NetworkStatus};
    use amafu_domain::kinds::network_endpoint::NetworkEndpointStatus;
    use amafu_domain::kinds::security_group::{
        Direction, PortRange, Protocol, SecurityGroupRule, SecurityGroupSpecification,
    };
    use amafu_domain::kinds::vm::{
        InterfaceStatus, NetworkInterfaceSpecification, VmSpecification, VmStatus,
    };
    use amafu_domain::{MacAddress, SegmentId};

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
            status: NetworkStatus {
                phase: NetworkPhase::Ready,
                segment_id: Some(SegmentId::new(7).unwrap()),
                ..NetworkStatus::default()
            },
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

    fn endpoint_of(vm: &Object<Vm>, node_address: [u8; 4]) -> Object<NetworkEndpoint> {
        let on = vm.status.node.clone().unwrap();
        let view = NetworkView {
            underlay_address: Some(Ipv4Addr::from(node_address)),
            ..view(&on, vec![vm.clone()])
        };
        let (reference, specification) = wanted_endpoints(&view).into_iter().next().unwrap();
        Object {
            metadata: metadata(reference.name.as_str(), Some("project-a")),
            specification,
            status: NetworkEndpointStatus {},
        }
    }

    fn view(on: &NodeName, vms: Vec<Object<Vm>>) -> NetworkView {
        NetworkView {
            node: on.clone(),
            underlay_address: Some(Ipv4Addr::new(192, 0, 2, 10)),
            vms,
            networks: vec![network()],
            groups: Vec::new(),
            endpoints: Vec::new(),
            floating_ips: Vec::new(),
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
        let state = desired_network_state(&view(&node("node-0"), vms));
        assert_eq!(state.ports.len(), 1);
        assert_eq!(state.ports[0].hostname, "web");
        assert_eq!(state.segments.len(), 1);
        assert_eq!(state.segments[0].domain, "default.project-a.internal");
        assert_eq!(state.segments[0].segment_id, Some(SegmentId::new(7).unwrap()));
        assert_eq!(state.underlay_address, Some(Ipv4Addr::new(192, 0, 2, 10)));
    }

    #[test]
    fn remote_ports_on_local_segments_are_reached_through_their_node() {
        let local = vm("web", "node-0", [10, 1, 0, 2], &[]);
        let remote = vm("database", "node-1", [10, 1, 0, 3], &[]);
        let mut observed = view(&node("node-0"), vec![local.clone(), remote.clone()]);
        observed.endpoints =
            vec![endpoint_of(&remote, [192, 0, 2, 11]), endpoint_of(&local, [192, 0, 2, 10])];
        let state = desired_network_state(&observed);
        assert_eq!(state.remote_endpoints.len(), 1);
        assert_eq!(state.remote_endpoints[0].hostname, "database");
        assert_eq!(state.remote_endpoints[0].node_address, Ipv4Addr::new(192, 0, 2, 11));
        let alone = view(&node("node-2"), vec![remote.clone()]);
        let mut unrelated = alone;
        unrelated.endpoints = vec![endpoint_of(&remote, [192, 0, 2, 11])];
        assert_eq!(desired_network_state(&unrelated).remote_endpoints, Vec::new());
    }

    #[test]
    fn security_group_remotes_include_members_on_other_nodes() {
        let client = vm("client", "node-1", [10, 1, 0, 3], &["clients"]);
        let mut observed = view(&node("node-0"), vec![vm("web", "node-0", [10, 1, 0, 2], &["web"])]);
        observed.groups = vec![web_group()];
        observed.endpoints = vec![endpoint_of(&client, [192, 0, 2, 11])];
        let state = desired_network_state(&observed);
        assert_eq!(
            state.ports[0].rules[0].remote,
            RemoteAddresses::Networks { networks: vec![Ipv4Network::host(Ipv4Addr::new(10, 1, 0, 3))] }
        );
    }

    #[test]
    fn groups_of_other_projects_are_ignored() {
        let mut foreign = web_group();
        foreign.metadata.project = Some(ProjectName::new("project-b").unwrap());
        let mut observed = view(&node("node-0"), vec![vm("web", "node-0", [10, 1, 0, 2], &["web"])]);
        observed.groups = vec![foreign];
        assert_eq!(desired_network_state(&observed).ports[0].rules, Vec::new());
    }

    #[test]
    fn a_floating_address_bound_here_is_served_here() {
        let web = vm("web", "node-0", [10, 1, 0, 2], &[]);
        let floating = |on: &str| Object {
            metadata: metadata("public", Some("project-a")),
            specification: FloatingIpSpecification {
                external_network: name("lab"),
                address: None,
                target: None,
            },
            status: FloatingIpStatus {
                phase: FloatingIpPhase::Bound,
                address: Some(Ipv4Addr::new(198, 51, 100, 7)),
                binding: Some(FloatingIpBinding {
                    node: node(on),
                    vm_id: web.metadata.id,
                    private_address: Ipv4Addr::new(10, 1, 0, 2),
                }),
                message: None,
            },
        };
        let mut observed = view(&node("node-0"), vec![web.clone()]);
        observed.floating_ips = vec![floating("node-0")];
        let state = desired_network_state(&observed);
        assert_eq!(state.floating_addresses.len(), 1);
        assert_eq!(state.floating_addresses[0].private_address, Ipv4Addr::new(10, 1, 0, 2));
        observed.floating_ips = vec![floating("node-1")];
        assert_eq!(desired_network_state(&observed).floating_addresses, Vec::new());
    }

    #[test]
    fn endpoints_are_published_once_realized_and_withdrawn_when_gone() {
        let web = vm("web", "node-0", [10, 1, 0, 2], &[]);
        let observed = view(&node("node-0"), vec![web.clone()]);
        let state = desired_network_state(&observed);
        let fresh = NodeNetworkObservation {
            desired: state.clone(),
            last_applied: Some(state.clone()),
            realized: RealizedNetwork {
                segments: state.segments.iter().map(|segment| segment.key.clone()).collect(),
                ports: state.ports.iter().map(|port| port.key).collect(),
            },
            wanted_endpoints: wanted_endpoints(&observed),
            published_endpoints: BTreeMap::new(),
        };
        assert!(matches!(plan_node_network(&fresh).as_slice(), [NodeNetworkAction::Publish { .. }]));
        let published =
            NodeNetworkObservation { published_endpoints: fresh.wanted_endpoints.clone(), ..fresh.clone() };
        assert_eq!(plan_node_network(&published), Vec::new());
        let gone = NodeNetworkObservation { wanted_endpoints: BTreeMap::new(), ..published };
        assert!(matches!(plan_node_network(&gone).as_slice(), [NodeNetworkAction::Withdraw { .. }]));
    }

    #[test]
    fn the_datapath_converges_once_until_something_changes() {
        let state =
            desired_network_state(&view(&node("node-0"), vec![vm("web", "node-0", [10, 1, 0, 2], &[])]));
        let observation = |last_applied, realized| NodeNetworkObservation {
            desired: state.clone(),
            last_applied,
            realized,
            wanted_endpoints: BTreeMap::new(),
            published_endpoints: BTreeMap::new(),
        };
        assert_eq!(
            plan_node_network(&observation(None, RealizedNetwork::default())),
            vec![NodeNetworkAction::Converge(state.clone())]
        );
        let realized = RealizedNetwork {
            segments: state.segments.iter().map(|segment| segment.key.clone()).collect(),
            ports: state.ports.iter().map(|port| port.key).collect(),
        };
        assert_eq!(plan_node_network(&observation(Some(state.clone()), realized)), Vec::new());
        assert_eq!(plan_node_network(&observation(Some(state.clone()), RealizedNetwork::default())).len(), 1);
    }
}
