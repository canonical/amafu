use std::sync::Arc;

use amafu_domain::kinds::network::Network;
use amafu_domain::kinds::node::Node;
use amafu_domain::kinds::vm::{InterfaceStatus, Vm, VmPhase, VmStatus};
use amafu_domain::{Change, NodeName, Object, ObjectReference};
use amafu_interfaces::objects::{apply, get, list, remove};
use amafu_interfaces::{ActionError, Consistency, Store};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};

use crate::address_allocation::{AllocationProblem, allocate_interfaces};

pub struct PlacementController {
    store: Arc<dyn Store>,
}

impl PlacementController {
    #[must_use]
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlacementObservation {
    pub vm: Option<Object<Vm>>,
    pub nodes: Vec<Object<Node>>,
    pub networks: Vec<Object<Network>>,
    pub peers: Vec<Object<Vm>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacementAction {
    Assign { node: NodeName, interfaces: Vec<InterfaceStatus>, observed_generation: u64 },
    ReportWaiting { observed_generation: u64, phase: VmPhase, reason: String },
    Remove,
}

const NO_NODE_AVAILABLE: &str = "no ready node accepts workloads";

#[must_use]
pub fn choose_node(nodes: &[Object<Node>]) -> Option<NodeName> {
    nodes
        .iter()
        .filter(|node| {
            !node.metadata.is_deleting && node.status.is_ready() && node.specification.accepts_workloads()
        })
        .map(|node| node.metadata.name.as_str())
        .min()
        .and_then(|name| NodeName::new(name).ok())
}

fn report_waiting(vm: &Object<Vm>, phase: VmPhase, reason: String) -> Vec<PlacementAction> {
    if vm.status.phase == phase && vm.status.message.as_ref() == Some(&reason) && vm.is_status_current() {
        return Vec::new();
    }
    vec![PlacementAction::ReportWaiting { observed_generation: vm.metadata.generation, phase, reason }]
}

#[must_use]
pub fn plan_placement(observation: &PlacementObservation) -> Vec<PlacementAction> {
    let Some(vm) = &observation.vm else {
        return Vec::new();
    };
    if vm.status.node.is_some() {
        return Vec::new();
    }
    if vm.metadata.is_deleting {
        return vec![PlacementAction::Remove];
    }
    let interfaces = match allocate_interfaces(vm, &observation.networks, &observation.peers) {
        Ok(interfaces) => interfaces,
        Err(AllocationProblem::NotYet(reason)) => return report_waiting(vm, VmPhase::Pending, reason),
        Err(AllocationProblem::Impossible(reason)) => return report_waiting(vm, VmPhase::Failed, reason),
    };
    match choose_node(&observation.nodes) {
        Some(node) => {
            vec![PlacementAction::Assign { node, interfaces, observed_generation: vm.metadata.generation }]
        }
        None => report_waiting(vm, VmPhase::Pending, NO_NODE_AVAILABLE.to_owned()),
    }
}

impl Controller for PlacementController {
    type Key = ObjectReference;
    type Observation = PlacementObservation;
    type Action = PlacementAction;

    const NAME: &'static str = "placement";
    const WATCHES: &'static [WatchedKind<ObjectReference>] = &[
        WatchedKind { kind: "Vm", mapping: KeyMapping::ObjectKey(|reference| Some(reference.clone())) },
        WatchedKind { kind: "Node", mapping: KeyMapping::EveryKey },
        WatchedKind { kind: "Network", mapping: KeyMapping::EveryKey },
    ];

    async fn discover_keys(&self) -> Result<Vec<ObjectReference>, ActionError> {
        let vms = list::<Vm>(self.store.as_ref(), None, Consistency::Linearizable).await?;
        Ok(vms.iter().filter(|vm| vm.status.node.is_none()).map(Object::reference).collect())
    }

    async fn observe(&self, key: &ObjectReference) -> Result<PlacementObservation, ActionError> {
        let store = self.store.as_ref();
        let vm = get::<Vm>(store, key, Consistency::Linearizable).await?;
        let nodes = list::<Node>(store, None, Consistency::Linearizable).await?;
        let (networks, peers) = match &vm {
            Some(vm) if !vm.specification.network_interfaces.is_empty() => {
                let project = vm.metadata.project.as_ref();
                (
                    list::<Network>(store, project, Consistency::Linearizable).await?,
                    list::<Vm>(store, project, Consistency::Linearizable).await?,
                )
            }
            _ => (Vec::new(), Vec::new()),
        };
        Ok(PlacementObservation { vm, nodes, networks, peers })
    }

    fn plan(&self, observation: &PlacementObservation) -> Vec<PlacementAction> {
        plan_placement(observation)
    }

    async fn apply(&self, key: &ObjectReference, action: PlacementAction) -> Result<(), ActionError> {
        let store = self.store.as_ref();
        match action {
            PlacementAction::Assign { node, interfaces, observed_generation } => {
                let status =
                    VmStatus { phase: VmPhase::Scheduled, node: Some(node), interfaces, message: None };
                apply::<Vm>(store, key, Change::UpdateStatus { observed_generation, status }).await?;
            }
            PlacementAction::ReportWaiting { observed_generation, phase, reason } => {
                let status = VmStatus { phase, node: None, interfaces: Vec::new(), message: Some(reason) };
                apply::<Vm>(store, key, Change::UpdateStatus { observed_generation, status }).await?;
            }
            PlacementAction::Remove => remove::<Vm>(store, key).await?,
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use amafu_domain::kinds::node::{NodePhase, NodeSpecification, NodeStatus};
    use amafu_domain::kinds::vm::VmSpecification;
    use amafu_domain::{Metadata, ObjectId, ObjectName, ProjectName};

    use super::*;

    fn metadata(name: &str, project: Option<&str>) -> Metadata {
        Metadata {
            name: ObjectName::new(name).unwrap(),
            project: project.map(|project| ProjectName::new(project).unwrap()),
            id: ObjectId::from_uuid(uuid::Uuid::new_v4()),
            version: 1,
            generation: 1,
            observed_generation: 0,
            labels: BTreeMap::new(),
            is_deleting: false,
        }
    }

    fn node(name: &str, phase: NodePhase, is_cordoned: bool) -> Object<Node> {
        Object {
            metadata: metadata(name, None),
            specification: NodeSpecification { is_cordoned, ..NodeSpecification::default() },
            status: NodeStatus { phase, ..NodeStatus::default() },
        }
    }

    fn vm() -> Object<Vm> {
        Object {
            metadata: metadata("web-1", Some("project-a")),
            specification: VmSpecification::small(),
            status: VmStatus::default(),
        }
    }

    fn plan(vm: Option<Object<Vm>>, nodes: Vec<Object<Node>>) -> Vec<PlacementAction> {
        plan_placement(&PlacementObservation { vm, nodes, networks: Vec::new(), peers: Vec::new() })
    }

    #[test]
    fn a_pending_vm_goes_to_the_first_ready_node_by_name() {
        let nodes = vec![node("node-b", NodePhase::Ready, false), node("node-a", NodePhase::Ready, false)];
        let actions = plan(Some(vm()), nodes);
        assert_eq!(
            actions,
            vec![PlacementAction::Assign {
                node: NodeName::new("node-a").unwrap(),
                interfaces: Vec::new(),
                observed_generation: 1
            }]
        );
    }

    #[test]
    fn cordoned_and_unready_nodes_are_skipped() {
        let nodes = vec![node("node-a", NodePhase::NotReady, false), node("node-b", NodePhase::Ready, true)];
        let actions = plan(Some(vm()), nodes);
        assert!(matches!(
            actions.as_slice(),
            [PlacementAction::ReportWaiting { phase: VmPhase::Pending, .. }]
        ));
    }

    #[test]
    fn unschedulable_is_reported_once() {
        let mut waiting = vm();
        waiting.status.message = Some(NO_NODE_AVAILABLE.to_owned());
        waiting.metadata.observed_generation = 1;
        assert_eq!(plan(Some(waiting), Vec::new()), Vec::new());
    }

    #[test]
    fn a_placed_vm_is_left_alone() {
        let mut placed = vm();
        placed.status.node = Some(NodeName::new("node-a").unwrap());
        assert_eq!(plan(Some(placed), vec![node("node-a", NodePhase::Ready, false)]), Vec::new());
    }

    #[test]
    fn an_unplaced_deleting_vm_is_removed() {
        let mut deleting = vm();
        deleting.metadata.is_deleting = true;
        assert_eq!(plan(Some(deleting), Vec::new()), vec![PlacementAction::Remove]);
    }

    #[test]
    fn a_missing_vm_needs_nothing() {
        assert_eq!(plan(None, vec![node("node-a", NodePhase::Ready, false)]), Vec::new());
    }
}
