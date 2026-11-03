use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use amafu_domain::kinds::image::Image;
use amafu_domain::kinds::network::Network;
use amafu_domain::kinds::node::{Node, NodeRole};
use amafu_domain::kinds::storage_backend::StorageBackend;
use amafu_domain::kinds::vm::{InterfaceStatus, Vm, VmPhase, VmStatus};
use amafu_domain::kinds::volume::Volume;
use amafu_domain::{Change, NodeName, Object, ObjectId, ObjectReference, Resource, ZoneName};
use amafu_interfaces::objects::{apply_fenced, list, remove};
use amafu_interfaces::{ActionError, Consistency, Fence, Store};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};
use amafu_scheduler::{CandidateNode, ClusterCapacity, PendingWorkload, PlacementDecision, Scheduler, place};

use crate::address_allocation::{AllocationProblem, allocate_interfaces};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchedulingSettings {
    pub cpu_overcommit_ratio: u32,
    pub reserved_memory_mebibytes: u64,
}

impl Default for SchedulingSettings {
    fn default() -> Self {
        Self { cpu_overcommit_ratio: 4, reserved_memory_mebibytes: 512 }
    }
}

pub struct SchedulerController {
    store: Arc<dyn Store>,
    fence: Fence,
    settings: SchedulingSettings,
}

impl SchedulerController {
    #[must_use]
    pub fn new(store: Arc<dyn Store>, fence: Fence, settings: SchedulingSettings) -> Self {
        Self { store, fence, settings }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PendingWorkloads;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SchedulingObservation {
    pub vms: Vec<Object<Vm>>,
    pub nodes: Vec<Object<Node>>,
    pub networks: Vec<Object<Network>>,
    pub volumes: Vec<Object<Volume>>,
    pub images: Vec<Object<Image>>,
    pub backends: Vec<Object<StorageBackend>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchedulingAction {
    Assign { vm: ObjectReference, node: NodeName, interfaces: Vec<InterfaceStatus>, observed_generation: u64 },
    ReportWaiting { vm: ObjectReference, observed_generation: u64, phase: VmPhase, reason: String },
    Remove { vm: ObjectReference },
}

fn same_project<'objects, R: Resource>(
    objects: &'objects [Object<R>],
    vm: &Object<Vm>,
) -> impl Iterator<Item = &'objects Object<R>> {
    let project = vm.metadata.project.clone();
    objects.iter().filter(move |object| object.metadata.project == project)
}

fn project_key(vm: &Object<Vm>) -> String {
    vm.metadata.project.as_ref().map(ToString::to_string).unwrap_or_default()
}

fn anti_affinity_key(vm: &Object<Vm>) -> Option<String> {
    vm.specification.anti_affinity_group.as_ref().map(|group| format!("{}/{group}", project_key(vm)))
}

fn report_waiting(vm: &Object<Vm>, phase: VmPhase, reason: String) -> Option<SchedulingAction> {
    if vm.status.phase == phase && vm.status.message.as_ref() == Some(&reason) && vm.is_status_current() {
        return None;
    }
    Some(SchedulingAction::ReportWaiting {
        vm: vm.reference(),
        observed_generation: vm.metadata.generation,
        phase,
        reason,
    })
}

struct Requirements {
    node: Option<NodeName>,
    images: BTreeSet<ObjectId>,
}

fn storage_requirements(
    vm: &Object<Vm>,
    observation: &SchedulingObservation,
) -> Result<Requirements, String> {
    let mut requirements = Requirements { node: None, images: BTreeSet::new() };
    if let Some(image) = vm.specification.container_image()
        && let Some(found) = same_project(&observation.images, vm).find(|found| found.metadata.name == *image)
    {
        requirements.images.insert(found.metadata.id);
    }
    for name in &vm.specification.volumes {
        let Some(volume) =
            same_project(&observation.volumes, vm).find(|volume| volume.metadata.name == *name)
        else {
            continue;
        };
        if let Some(pinned) =
            volume.status.provisioned.as_ref().and_then(|provisioned| provisioned.node.clone())
        {
            if requirements.node.as_ref().is_some_and(|required| *required != pinned) {
                return Err(format!("its volumes live on different nodes ({pinned} and others)"));
            }
            requirements.node = Some(pinned);
        }
        let is_file_backed = observation
            .backends
            .iter()
            .find(|backend| backend.metadata.name == volume.specification.backend)
            .is_some_and(|backend| backend.specification.provider.is_file_based());
        if is_file_backed
            && let Some(source) = &volume.specification.source_image
            && let Some(image) =
                same_project(&observation.images, vm).find(|image| image.metadata.name == *source)
        {
            requirements.images.insert(image.metadata.id);
        }
    }
    Ok(requirements)
}

fn workload(vm: &Object<Vm>, requirements: Requirements) -> PendingWorkload {
    let mut workload = PendingWorkload::new(
        vm.metadata.id,
        u32::from(vm.specification.cpu_count),
        vm.specification.memory_mebibytes,
    );
    workload.priority = vm.specification.priority;
    workload.minimum_cpu_baseline = vm.specification.minimum_cpu_baseline.unwrap_or_default();
    workload.required_node = requirements.node;
    workload.required_images = requirements.images;
    workload.anti_affinity_group = anti_affinity_key(vm);
    workload.spread_group = workload.anti_affinity_group.clone().unwrap_or_else(|| project_key(vm));
    workload
}

fn cluster_capacity(observation: &SchedulingObservation, settings: SchedulingSettings) -> ClusterCapacity {
    let candidates = observation
        .nodes
        .iter()
        .filter(|node| {
            !node.metadata.is_deleting
                && node.status.is_ready()
                && node.specification.roles.contains(&NodeRole::Compute)
        })
        .filter_map(|node| {
            let name = NodeName::new(node.metadata.name.as_str()).ok()?;
            let capacity = node.status.capacity;
            let mut candidate = CandidateNode::new(
                name.clone(),
                capacity.cpu_count.saturating_mul(settings.cpu_overcommit_ratio),
                capacity.memory_mebibytes.saturating_sub(settings.reserved_memory_mebibytes),
            );
            candidate.zone.clone_from(&node.specification.zone);
            candidate.is_cordoned = node.specification.is_cordoned;
            candidate.cpu_baseline = node.status.cpu_baseline;
            candidate.cached_images = observation
                .images
                .iter()
                .filter(|image| image.status.is_ready() && image.status.cached_on.contains(&name))
                .map(|image| image.metadata.id)
                .collect();
            for vm in observation.vms.iter().filter(|vm| vm.status.node.as_ref() == Some(&name)) {
                candidate.allocated_cpu_count += u32::from(vm.specification.cpu_count);
                candidate.allocated_memory_mebibytes += vm.specification.memory_mebibytes;
                candidate.anti_affinity_groups.extend(anti_affinity_key(vm));
            }
            Some(candidate)
        })
        .collect();
    let mut cluster = ClusterCapacity::new(candidates);
    let zones: BTreeMap<String, Option<ZoneName>> = observation
        .nodes
        .iter()
        .map(|node| (node.metadata.name.to_string(), node.specification.zone.clone()))
        .collect();
    for vm in &observation.vms {
        if let Some(node) = &vm.status.node {
            let zone = zones.get(node.as_str()).cloned().flatten();
            let spread_group = anti_affinity_key(vm).unwrap_or_else(|| project_key(vm));
            cluster.count_in_zone(zone.as_ref(), &spread_group);
        }
    }
    cluster
}

#[must_use]
pub fn plan_scheduling(
    observation: &SchedulingObservation,
    settings: SchedulingSettings,
) -> Vec<SchedulingAction> {
    let mut actions = Vec::new();
    let mut assigned_peers: Vec<Object<Vm>> = observation.vms.clone();
    let mut pending = Vec::new();
    let mut allocations: BTreeMap<ObjectId, (Object<Vm>, Vec<InterfaceStatus>)> = BTreeMap::new();
    let unplaced = observation.vms.iter().filter(|vm| vm.status.node.is_none());
    for vm in unplaced {
        if vm.metadata.is_deleting {
            actions.push(SchedulingAction::Remove { vm: vm.reference() });
            continue;
        }
        let networks: Vec<Object<Network>> = same_project(&observation.networks, vm).cloned().collect();
        let peers: Vec<Object<Vm>> = same_project(&assigned_peers, vm).cloned().collect();
        let interfaces = match allocate_interfaces(vm, &networks, &peers) {
            Ok(interfaces) => interfaces,
            Err(AllocationProblem::NotYet(reason)) => {
                actions.extend(report_waiting(vm, VmPhase::Pending, reason));
                continue;
            }
            Err(AllocationProblem::Impossible(reason)) => {
                actions.extend(report_waiting(vm, VmPhase::Failed, reason));
                continue;
            }
        };
        let requirements = match storage_requirements(vm, observation) {
            Ok(requirements) => requirements,
            Err(reason) => {
                actions.extend(report_waiting(vm, VmPhase::Failed, reason));
                continue;
            }
        };
        let mut reserved = vm.clone();
        reserved.status.interfaces.clone_from(&interfaces);
        assigned_peers.retain(|peer| peer.metadata.id != vm.metadata.id);
        assigned_peers.push(reserved);
        pending.push(workload(vm, requirements));
        allocations.insert(vm.metadata.id, (vm.clone(), interfaces));
    }
    for decision in place(&Scheduler::standard(), cluster_capacity(observation, settings), pending) {
        match decision {
            PlacementDecision::Place(id, node) => {
                if let Some((vm, interfaces)) = allocations.remove(&id) {
                    actions.push(SchedulingAction::Assign {
                        vm: vm.reference(),
                        node,
                        interfaces,
                        observed_generation: vm.metadata.generation,
                    });
                }
            }
            PlacementDecision::Unschedulable(id, reason) => {
                if let Some((vm, _)) = allocations.get(&id) {
                    actions.extend(report_waiting(vm, VmPhase::Pending, reason));
                }
            }
        }
    }
    actions
}

impl Controller for SchedulerController {
    type Key = PendingWorkloads;
    type Observation = SchedulingObservation;
    type Action = SchedulingAction;

    const NAME: &'static str = "scheduler";
    const WATCHES: &'static [WatchedKind<PendingWorkloads>] = &[
        WatchedKind { kind: "Vm", mapping: KeyMapping::EveryKey },
        WatchedKind { kind: "Node", mapping: KeyMapping::EveryKey },
        WatchedKind { kind: "Network", mapping: KeyMapping::EveryKey },
        WatchedKind { kind: "Volume", mapping: KeyMapping::EveryKey },
        WatchedKind { kind: "Image", mapping: KeyMapping::EveryKey },
    ];

    async fn discover_keys(&self) -> Result<Vec<PendingWorkloads>, ActionError> {
        Ok(vec![PendingWorkloads])
    }

    async fn observe(&self, _key: &PendingWorkloads) -> Result<SchedulingObservation, ActionError> {
        let store = self.store.as_ref();
        let consistency = Consistency::Linearizable;
        Ok(SchedulingObservation {
            vms: list::<Vm>(store, None, consistency).await?,
            nodes: list::<Node>(store, None, consistency).await?,
            networks: list::<Network>(store, None, consistency).await?,
            volumes: list::<Volume>(store, None, consistency).await?,
            images: list::<Image>(store, None, consistency).await?,
            backends: list::<StorageBackend>(store, None, consistency).await?,
        })
    }

    fn plan(&self, observation: &SchedulingObservation) -> Vec<SchedulingAction> {
        plan_scheduling(observation, self.settings)
    }

    async fn apply(&self, _key: &PendingWorkloads, action: SchedulingAction) -> Result<(), ActionError> {
        let store = self.store.as_ref();
        match action {
            SchedulingAction::Assign { vm, node, interfaces, observed_generation } => {
                let status =
                    VmStatus { phase: VmPhase::Scheduled, node: Some(node), interfaces, message: None };
                apply_fenced::<Vm>(
                    store,
                    &vm,
                    &self.fence,
                    Change::UpdateStatus { observed_generation, status },
                )
                .await?;
            }
            SchedulingAction::ReportWaiting { vm, observed_generation, phase, reason } => {
                let status = VmStatus { phase, node: None, interfaces: Vec::new(), message: Some(reason) };
                apply_fenced::<Vm>(
                    store,
                    &vm,
                    &self.fence,
                    Change::UpdateStatus { observed_generation, status },
                )
                .await?;
            }
            SchedulingAction::Remove { vm } => remove::<Vm>(store, &vm).await?,
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use amafu_domain::kinds::image::{ImagePhase, ImageSource, ImageSpecification, ImageStatus};
    use amafu_domain::kinds::network::{NetworkPhase, NetworkSpecification, NetworkStatus};
    use amafu_domain::kinds::node::{NodeCapacity, NodePhase, NodeSpecification, NodeStatus};
    use amafu_domain::kinds::storage_backend::{StorageBackendSpecification, StorageProviderSpecification};
    use amafu_domain::kinds::vm::{NetworkInterfaceSpecification, VmSpecification};
    use amafu_domain::kinds::volume::{ProvisionedVolume, VolumeSpecification};

    use super::*;
    use crate::testing::{metadata, name};

    fn node_name(value: &str) -> NodeName {
        NodeName::new(value).unwrap()
    }

    fn node(value: &str, memory_mebibytes: u64) -> Object<Node> {
        Object {
            metadata: metadata(value, None),
            specification: NodeSpecification::default(),
            status: NodeStatus {
                phase: NodePhase::Ready,
                capacity: NodeCapacity { cpu_count: 4, memory_mebibytes },
                ..NodeStatus::default()
            },
        }
    }

    fn vm(value: &str) -> Object<Vm> {
        Object {
            metadata: metadata(value, Some("project-a")),
            specification: VmSpecification::small(),
            status: VmStatus::default(),
        }
    }

    fn placed(value: &str, on: &str) -> Object<Vm> {
        let mut vm = vm(value);
        vm.status.node = Some(node_name(on));
        vm
    }

    fn assigned_nodes(actions: &[SchedulingAction]) -> BTreeMap<String, String> {
        actions
            .iter()
            .filter_map(|action| match action {
                SchedulingAction::Assign { vm, node, .. } => Some((vm.name.to_string(), node.to_string())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn pending_vms_spread_over_the_least_allocated_nodes() {
        let observation = SchedulingObservation {
            vms: vec![placed("old", "node-a"), vm("web-1"), vm("web-2")],
            nodes: vec![node("node-a", 4096), node("node-b", 4096)],
            ..SchedulingObservation::default()
        };
        let assigned = assigned_nodes(&plan_scheduling(&observation, SchedulingSettings::default()));
        assert_eq!(assigned.get("web-1").map(String::as_str), Some("node-b"));
        assert_eq!(assigned.len(), 2);
    }

    #[test]
    fn a_vm_that_fits_nowhere_waits_with_the_reason() {
        let mut huge = vm("huge");
        huge.specification.memory_mebibytes = 64 * 1024;
        let observation = SchedulingObservation {
            vms: vec![huge],
            nodes: vec![node("node-a", 4096)],
            ..SchedulingObservation::default()
        };
        let actions = plan_scheduling(&observation, SchedulingSettings::default());
        assert!(matches!(
            actions.as_slice(),
            [SchedulingAction::ReportWaiting { phase: VmPhase::Pending, reason, .. }]
                if reason == "no node fits: 1 node is without enough free CPU or memory"
        ));
    }

    #[test]
    fn vms_of_one_batch_get_distinct_addresses() {
        let network = Object {
            metadata: metadata("default", Some("project-a")),
            specification: NetworkSpecification {
                subnet: "10.9.0.0/29".parse().unwrap(),
                is_outbound_access_allowed: true,
            },
            status: NetworkStatus { phase: NetworkPhase::Ready, ..NetworkStatus::default() },
        };
        let with_interface = |value: &str| {
            let mut vm = vm(value);
            vm.specification.network_interfaces = vec![NetworkInterfaceSpecification {
                network: name("default"),
                security_groups: Vec::new(),
                address: None,
            }];
            vm
        };
        let observation = SchedulingObservation {
            vms: vec![with_interface("web-1"), with_interface("web-2")],
            nodes: vec![node("node-a", 4096)],
            networks: vec![network],
            ..SchedulingObservation::default()
        };
        let addresses: BTreeSet<_> = plan_scheduling(&observation, SchedulingSettings::default())
            .into_iter()
            .filter_map(|action| match action {
                SchedulingAction::Assign { interfaces, .. } => Some(interfaces[0].address),
                _ => None,
            })
            .collect();
        assert_eq!(addresses.len(), 2);
    }

    #[test]
    fn a_vm_follows_its_local_volume_and_waits_for_its_image() {
        let backend = Object {
            metadata: metadata("local", None),
            specification: StorageBackendSpecification { provider: StorageProviderSpecification::LocalDisk },
            status: Default::default(),
        };
        let image = Object {
            metadata: metadata("noble", Some("project-a")),
            specification: ImageSpecification {
                source: ImageSource::Disk {
                    url: String::from("https://example.com/noble.img"),
                    sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                        .parse()
                        .unwrap(),
                },
            },
            status: ImageStatus {
                phase: ImagePhase::Ready,
                cached_on: BTreeSet::from([node_name("node-c")]),
                ..ImageStatus::default()
            },
        };
        let mut pinned: Object<Volume> = Object {
            metadata: metadata("data", Some("project-a")),
            specification: VolumeSpecification {
                size_gibibytes: 1,
                backend: name("local"),
                source_image: None,
            },
            status: Default::default(),
        };
        pinned.status.provisioned = Some(ProvisionedVolume {
            identifier: String::from("data"),
            size_bytes: 1,
            node: Some(node_name("node-b")),
        });
        let mut root = pinned.clone();
        root.metadata.name = name("root");
        root.specification.source_image = Some(name("noble"));
        root.status.provisioned = None;
        let mut database = vm("database");
        database.specification.volumes = vec![name("data")];
        let mut ubuntu = vm("ubuntu");
        ubuntu.specification.volumes = vec![name("root")];
        let observation = SchedulingObservation {
            vms: vec![database, ubuntu],
            nodes: vec![node("node-a", 8192), node("node-b", 4096), node("node-c", 4096)],
            volumes: vec![pinned, root],
            images: vec![image],
            backends: vec![backend],
            ..SchedulingObservation::default()
        };
        let assigned = assigned_nodes(&plan_scheduling(&observation, SchedulingSettings::default()));
        assert_eq!(assigned.get("database").map(String::as_str), Some("node-b"));
        assert_eq!(assigned.get("ubuntu").map(String::as_str), Some("node-c"));
    }

    #[test]
    fn unplaced_deleting_vms_are_removed_and_placed_ones_left_alone() {
        let mut deleting = vm("gone");
        deleting.metadata.is_deleting = true;
        let observation = SchedulingObservation {
            vms: vec![deleting, placed("web-1", "node-a")],
            nodes: vec![node("node-a", 4096)],
            ..SchedulingObservation::default()
        };
        let actions = plan_scheduling(&observation, SchedulingSettings::default());
        assert!(
            matches!(actions.as_slice(), [SchedulingAction::Remove { vm }] if vm.name.as_str() == "gone")
        );
    }

    #[test]
    fn a_waiting_report_is_written_once() {
        let mut waiting = vm("web-1");
        waiting.status.message = Some(String::from("no node accepts workloads"));
        waiting.metadata.observed_generation = 1;
        let observation = SchedulingObservation { vms: vec![waiting], ..SchedulingObservation::default() };
        assert_eq!(plan_scheduling(&observation, SchedulingSettings::default()), Vec::new());
    }
}
