use std::collections::BTreeSet;
use std::path::PathBuf;

use amafu_domain::kinds::image::Image;
use amafu_domain::kinds::storage_backend::StorageProviderSpecification;
use amafu_domain::kinds::vm::{BootSource, Vm, VmPhase, VmStatus};
use amafu_domain::kinds::volume::{ProvisionedVolume, Volume};
use amafu_domain::{NodeName, Object, ObjectId, ObjectName};
use amafu_interfaces::{
    CachedImage, InstanceState, LaunchInterface, Payload, PortKey, RunningInstance, VmLaunchSpecification,
    WorkloadSpecification, WorkloadState,
};

pub const METADATA_SEED: &str = "ds=nocloud;s=http://169.254.169.254/";

#[derive(Debug, Clone, PartialEq)]
pub struct ObservedVolume {
    pub name: ObjectName,
    pub volume: Option<Object<Volume>>,
    pub backend: Option<StorageProviderSpecification>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObservedImage {
    pub image: Option<Object<Image>>,
    pub cached: Option<CachedImage>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VmContext {
    pub node: NodeName,
    pub vm: Object<Vm>,
    pub instance: Option<RunningInstance>,
    pub workload: Option<WorkloadState>,
    pub volumes: Vec<ObservedVolume>,
    pub container_image: Option<ObservedImage>,
    pub realized_ports: BTreeSet<PortKey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeAttachment {
    pub provisioned: ProvisionedVolume,
    pub backend: StorageProviderSpecification,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartPlan {
    pub launch: VmLaunchSpecification,
    pub root_disk: Option<PathBuf>,
    pub volumes: Vec<VolumeAttachment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComputeAction {
    StartVm(Box<StartPlan>),
    StartWorkload { vm_id: ObjectId, workload: Box<WorkloadSpecification> },
    StopVm(ObjectId),
    ReleaseVolumes(Vec<VolumeAttachment>),
    ReportPhase { observed_generation: u64, status: VmStatus },
    Remove,
}

fn report(
    vm: &Object<Vm>,
    node: &NodeName,
    phase: VmPhase,
    message: Option<String>,
) -> Option<ComputeAction> {
    let status =
        VmStatus { phase, node: Some(node.clone()), interfaces: vm.status.interfaces.clone(), message };
    if vm.status == status && vm.is_status_current() {
        return None;
    }
    Some(ComputeAction::ReportPhase { observed_generation: vm.metadata.generation, status })
}

fn attachments(context: &VmContext) -> Result<Vec<VolumeAttachment>, String> {
    let vm_name = &context.vm.metadata.name;
    context
        .volumes
        .iter()
        .map(|observed| {
            let volume =
                observed.volume.as_ref().ok_or_else(|| format!("volume {} does not exist", observed.name))?;
            if !volume.status.is_reserved_for(vm_name) {
                return Err(format!("volume {} is not available to this VM yet", observed.name));
            }
            let provisioned = volume
                .status
                .provisioned
                .clone()
                .ok_or_else(|| format!("volume {} is not provisioned", observed.name))?;
            let backend = observed
                .backend
                .clone()
                .ok_or_else(|| format!("the backend of volume {} is missing", observed.name))?;
            Ok(VolumeAttachment { provisioned, backend })
        })
        .collect()
}

fn container_root(context: &VmContext) -> Result<Option<&CachedImage>, String> {
    let Some(image_name) = context.vm.specification.container_image() else {
        return Ok(None);
    };
    let observed = context.container_image.as_ref();
    let image = observed
        .and_then(|observed| observed.image.as_ref())
        .ok_or_else(|| format!("image {image_name} does not exist"))?;
    if !image.specification.is_container() {
        return Err(format!("image {image_name} is not a container image"));
    }
    let cached = observed.and_then(|observed| observed.cached.as_ref()).filter(|_| image.status.is_ready());
    cached.map(Some).ok_or_else(|| format!("image {image_name} is not ready on this node"))
}

fn port_keys(vm: &Object<Vm>) -> impl Iterator<Item = PortKey> + '_ {
    (0..vm.status.interfaces.len())
        .map(|index| PortKey { vm_id: vm.metadata.id, index: u8::try_from(index).unwrap_or(u8::MAX) })
}

fn launch_specification(vm: &Object<Vm>) -> VmLaunchSpecification {
    let payload = match &vm.specification.boot {
        BootSource::Kernel { bundle } => Payload::Bundle { name: bundle.clone() },
        BootSource::Disk => Payload::Firmware,
        BootSource::Container { .. } => Payload::ContainerTemplate,
    };
    let network_interfaces = port_keys(vm)
        .zip(&vm.status.interfaces)
        .map(|(key, interface)| LaunchInterface { tap: key.tap_name(), mac: interface.mac })
        .collect();
    let has_interfaces = !vm.status.interfaces.is_empty();
    VmLaunchSpecification {
        vm_id: vm.metadata.id,
        cpu_count: vm.specification.cpu_count,
        memory_mebibytes: vm.specification.memory_mebibytes,
        payload,
        disks: Vec::new(),
        network_interfaces,
        has_guest_agent: vm.specification.is_container(),
        metadata_seed: (has_interfaces && !vm.specification.is_container()).then(|| METADATA_SEED.to_owned()),
    }
}

fn start_plan(context: &VmContext) -> Result<StartPlan, String> {
    let volumes = attachments(context)?;
    let root = container_root(context)?;
    if let Some(port) = port_keys(&context.vm).find(|port| !context.realized_ports.contains(port)) {
        return Err(format!("network port {} is not connected yet", port.tap_name()));
    }
    Ok(StartPlan {
        launch: launch_specification(&context.vm),
        root_disk: root.map(|cached| cached.path.clone()),
        volumes,
    })
}

fn workload_of(context: &VmContext) -> Option<WorkloadSpecification> {
    let BootSource::Container { command, environment, .. } = &context.vm.specification.boot else {
        return None;
    };
    let configuration =
        context.container_image.as_ref()?.cached.as_ref()?.container.clone().unwrap_or_default();
    Some(WorkloadSpecification::from_image(
        &configuration,
        command,
        environment,
        context.vm.metadata.name.as_str(),
    ))
}

fn has_settled_at_this_generation(vm: &Object<Vm>) -> bool {
    let has_ended = match vm.status.phase {
        VmPhase::Failed => true,
        VmPhase::Stopped => vm.specification.is_container() && vm.status.message.is_some(),
        _ => false,
    };
    has_ended && vm.is_status_current()
}

fn plan_running_container(context: &VmContext) -> Vec<ComputeAction> {
    let (vm, node) = (&context.vm, &context.node);
    match context.workload {
        None => report(vm, node, VmPhase::Starting, Some(String::from("waiting for the guest agent")))
            .into_iter()
            .collect(),
        Some(WorkloadState::NotStarted) => match workload_of(context) {
            Some(workload) => {
                vec![ComputeAction::StartWorkload { vm_id: vm.metadata.id, workload: Box::new(workload) }]
            }
            None => report(
                vm,
                node,
                VmPhase::Failed,
                Some(String::from("the container image has no configuration")),
            )
            .into_iter()
            .collect(),
        },
        Some(WorkloadState::Running) => report(vm, node, VmPhase::Running, None).into_iter().collect(),
        Some(WorkloadState::Exited { code }) => {
            let phase = if code == 0 { VmPhase::Stopped } else { VmPhase::Failed };
            let mut actions: Vec<ComputeAction> =
                report(vm, node, phase, Some(format!("the workload exited with code {code}")))
                    .into_iter()
                    .collect();
            actions.push(ComputeAction::StopVm(vm.metadata.id));
            actions
        }
    }
}

fn plan_stopped(context: &VmContext) -> Vec<ComputeAction> {
    let vm = &context.vm;
    let held: Vec<VolumeAttachment> = attachments(context).unwrap_or_default();
    let needs_release = !held.is_empty() && (vm.metadata.is_deleting || vm.status.phase != VmPhase::Stopped);
    let mut actions = Vec::new();
    if needs_release {
        actions.push(ComputeAction::ReleaseVolumes(held));
    }
    if vm.metadata.is_deleting {
        actions.push(ComputeAction::Remove);
    } else {
        actions.extend(report(vm, &context.node, VmPhase::Stopped, None));
    }
    actions
}

#[must_use]
pub fn is_in_transition(context: &VmContext) -> bool {
    let should_run = context.vm.specification.should_run() && !context.vm.metadata.is_deleting;
    match context.instance.as_ref().map(|instance| instance.state) {
        None => should_run && !has_settled_at_this_generation(&context.vm),
        Some(InstanceState::Booting | InstanceState::Exited) => true,
        Some(InstanceState::Running) => {
            context.vm.specification.is_container() && context.workload != Some(WorkloadState::Running)
        }
    }
}

#[must_use]
pub fn plan_vm(context: &VmContext) -> Vec<ComputeAction> {
    let (vm, node) = (&context.vm, &context.node);
    let should_run = vm.specification.should_run() && !vm.metadata.is_deleting;
    match (should_run, context.instance.as_ref().map(|instance| instance.state)) {
        (true, None) if has_settled_at_this_generation(vm) => Vec::new(),
        (true, None) => match start_plan(context) {
            Ok(plan) => {
                let mut actions = vec![ComputeAction::StartVm(Box::new(plan))];
                actions.extend(report(vm, node, VmPhase::Starting, vm.status.message.clone()));
                actions
            }
            Err(reason) => report(vm, node, VmPhase::Scheduled, Some(reason)).into_iter().collect(),
        },
        (true, Some(InstanceState::Booting)) => {
            report(vm, node, VmPhase::Starting, vm.status.message.clone()).into_iter().collect()
        }
        (true, Some(InstanceState::Running)) if vm.specification.is_container() => {
            plan_running_container(context)
        }
        (true, Some(InstanceState::Running)) => {
            report(vm, node, VmPhase::Running, None).into_iter().collect()
        }
        (_, Some(InstanceState::Exited)) => plan_exited(context),
        (false, Some(InstanceState::Booting | InstanceState::Running)) => {
            let mut actions: Vec<ComputeAction> =
                report(vm, node, VmPhase::Stopping, None).into_iter().collect();
            actions.push(ComputeAction::StopVm(vm.metadata.id));
            actions
        }
        (false, None) => plan_stopped(context),
    }
}

fn plan_exited(context: &VmContext) -> Vec<ComputeAction> {
    let (vm, node) = (&context.vm, &context.node);
    let failure = context.instance.as_ref().and_then(|instance| instance.failure.clone());
    let mut actions: Vec<ComputeAction> = match failure {
        Some(failure) if vm.specification.should_run() && !vm.metadata.is_deleting => {
            report(vm, node, VmPhase::Starting, Some(format!("the VM process failed: {failure}")))
                .into_iter()
                .collect()
        }
        _ => Vec::new(),
    };
    actions.push(ComputeAction::StopVm(vm.metadata.id));
    actions
}

#[must_use]
pub fn plan_stray(instance: Option<&RunningInstance>) -> Vec<ComputeAction> {
    instance.map(|instance| ComputeAction::StopVm(instance.vm_id)).into_iter().collect()
}

#[must_use]
pub fn plan_orphans(instances: &[RunningInstance], assigned: &BTreeSet<ObjectId>) -> Vec<ComputeAction> {
    instances
        .iter()
        .filter(|instance| !assigned.contains(&instance.vm_id))
        .map(|instance| ComputeAction::StopVm(instance.vm_id))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use amafu_domain::MacAddress;
    use amafu_domain::kinds::image::{ImagePhase, ImageSource, ImageSpecification, ImageStatus};
    use amafu_domain::kinds::vm::{
        InterfaceStatus, NetworkInterfaceSpecification, PowerState, VmSpecification,
    };
    use amafu_domain::kinds::volume::{VolumePhase, VolumeSpecification, VolumeStatus};
    use amafu_interfaces::ContainerConfiguration;

    use super::*;
    use crate::testing::{metadata, name, only};

    fn here() -> NodeName {
        NodeName::new("node-a").unwrap()
    }

    fn vm_on(node: Option<&str>, phase: VmPhase) -> Object<Vm> {
        Object {
            metadata: metadata("web-1", Some("project-a")),
            specification: VmSpecification::small(),
            status: VmStatus {
                phase,
                node: node.map(|node| NodeName::new(node).unwrap()),
                ..VmStatus::default()
            },
        }
    }

    fn context(vm: Object<Vm>, state: Option<InstanceState>) -> VmContext {
        let instance = state.map(|state| RunningInstance::new(vm.metadata.id, state));
        VmContext {
            node: here(),
            vm,
            instance,
            workload: None,
            volumes: Vec::new(),
            container_image: None,
            realized_ports: BTreeSet::new(),
        }
    }

    fn reported(vm: &Object<Vm>, phase: VmPhase) -> ComputeAction {
        ComputeAction::ReportPhase {
            observed_generation: vm.metadata.generation,
            status: VmStatus {
                phase,
                node: Some(here()),
                interfaces: vm.status.interfaces.clone(),
                message: None,
            },
        }
    }

    fn reserved_volume(vm_name: &str) -> ObservedVolume {
        let volume = Object {
            metadata: metadata("data", Some("project-a")),
            specification: VolumeSpecification {
                size_gibibytes: 1,
                backend: name("local"),
                source_image: None,
            },
            status: VolumeStatus {
                phase: VolumePhase::Attached,
                provisioned: Some(ProvisionedVolume {
                    identifier: String::from("data"),
                    size_bytes: 1,
                    node: None,
                }),
                attached_to: Some(name(vm_name)),
                message: None,
            },
        };
        ObservedVolume {
            name: name("data"),
            volume: Some(volume),
            backend: Some(StorageProviderSpecification::LocalDisk),
        }
    }

    fn container_vm() -> Object<Vm> {
        let mut vm = vm_on(Some("node-a"), VmPhase::Scheduled);
        vm.specification = VmSpecification::container(name("busybox"));
        vm
    }

    fn container_image(phase: ImagePhase) -> ObservedImage {
        let reference = "docker.io/library/busybox@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".parse().unwrap();
        ObservedImage {
            image: Some(Object {
                metadata: metadata("busybox", Some("project-a")),
                specification: ImageSpecification { source: ImageSource::Container { reference } },
                status: ImageStatus { phase, ..ImageStatus::default() },
            }),
            cached: Some(CachedImage {
                path: "/var/lib/amafu/images/busybox.erofs".into(),
                size_bytes: 1,
                container: Some(ContainerConfiguration {
                    command: vec![String::from("sh")],
                    ..ContainerConfiguration::default()
                }),
            }),
        }
    }

    #[test]
    fn a_scheduled_vm_is_started_and_reported_starting() {
        let vm = vm_on(Some("node-a"), VmPhase::Scheduled);
        let actions = plan_vm(&context(vm.clone(), None));
        assert!(
            matches!(&actions[0], ComputeAction::StartVm(plan) if plan.launch.payload == Payload::Bundle { name: name("test-guest") })
        );
        assert_eq!(actions[1], reported(&vm, VmPhase::Starting));
    }

    #[test]
    fn a_booting_then_running_instance_is_reported() {
        let vm = vm_on(Some("node-a"), VmPhase::Scheduled);
        assert_eq!(
            plan_vm(&context(vm.clone(), Some(InstanceState::Booting))),
            vec![reported(&vm, VmPhase::Starting)]
        );
        let starting = vm_on(Some("node-a"), VmPhase::Starting);
        assert_eq!(
            plan_vm(&context(starting.clone(), Some(InstanceState::Running))),
            vec![reported(&starting, VmPhase::Running)]
        );
        let running = vm_on(Some("node-a"), VmPhase::Running);
        assert_eq!(plan_vm(&context(running, Some(InstanceState::Running))), Vec::new());
    }

    #[test]
    fn a_new_generation_is_acknowledged_even_without_a_phase_change() {
        let mut vm = vm_on(Some("node-a"), VmPhase::Running);
        vm.metadata.generation = 2;
        assert_eq!(
            plan_vm(&context(vm.clone(), Some(InstanceState::Running))),
            vec![reported(&vm, VmPhase::Running)]
        );
    }

    #[test]
    fn a_stray_instance_is_stopped() {
        let vm = vm_on(Some("node-b"), VmPhase::Running);
        let stray = RunningInstance::new(vm.metadata.id, InstanceState::Running);
        assert_eq!(plan_stray(Some(&stray)), vec![ComputeAction::StopVm(vm.metadata.id)]);
        assert_eq!(plan_stray(None), Vec::new());
    }

    #[test]
    fn a_vm_powered_off_is_reported_stopping_then_stopped() {
        let mut vm = vm_on(Some("node-a"), VmPhase::Running);
        vm.specification.power_state = PowerState::Stopped;
        assert_eq!(
            plan_vm(&context(vm.clone(), Some(InstanceState::Running))),
            vec![reported(&vm, VmPhase::Stopping), ComputeAction::StopVm(vm.metadata.id)]
        );
        assert_eq!(plan_vm(&context(vm.clone(), None)), vec![reported(&vm, VmPhase::Stopped)]);
    }

    #[test]
    fn a_deleted_vm_is_stopped_releases_its_volumes_then_is_removed() {
        let mut vm = vm_on(Some("node-a"), VmPhase::Stopping);
        vm.metadata.is_deleting = true;
        vm.specification.volumes = vec![name("data")];
        assert_eq!(
            plan_vm(&context(vm.clone(), Some(InstanceState::Running))),
            vec![ComputeAction::StopVm(vm.metadata.id)]
        );
        let mut stopped = context(vm, None);
        stopped.volumes = vec![reserved_volume("web-1")];
        let actions = plan_vm(&stopped);
        assert!(
            matches!(actions.as_slice(), [ComputeAction::ReleaseVolumes(held), ComputeAction::Remove] if held.len() == 1)
        );
    }

    #[test]
    fn a_vm_waits_until_its_volume_is_reserved_for_it() {
        let mut vm = vm_on(Some("node-a"), VmPhase::Scheduled);
        vm.specification.volumes = vec![name("data")];
        let mut waiting = context(vm.clone(), None);
        waiting.volumes = vec![reserved_volume("someone-else")];
        assert!(matches!(
            plan_vm(&waiting).as_slice(),
            [ComputeAction::ReportPhase { status, .. }] if status.phase == VmPhase::Scheduled && status.message.is_some()
        ));
        waiting.volumes = vec![reserved_volume("web-1")];
        assert!(
            matches!(plan_vm(&waiting).as_slice(), [ComputeAction::StartVm(plan), _] if plan.volumes.len() == 1)
        );
    }

    #[test]
    fn a_vm_waits_until_its_ports_are_connected() {
        let mut vm = vm_on(Some("node-a"), VmPhase::Scheduled);
        vm.specification.network_interfaces = vec![NetworkInterfaceSpecification {
            network: name("default"),
            security_groups: Vec::new(),
            address: None,
        }];
        vm.status.interfaces = vec![InterfaceStatus {
            network: name("default"),
            mac: MacAddress::locally_administered(b"web-1"),
            address: Ipv4Addr::new(10, 1, 0, 2),
        }];
        let mut waiting = context(vm.clone(), None);
        assert!(matches!(plan_vm(&waiting).as_slice(), [ComputeAction::ReportPhase { .. }]));
        waiting.realized_ports.insert(PortKey { vm_id: vm.metadata.id, index: 0 });
        let actions = plan_vm(&waiting);
        let ComputeAction::StartVm(plan) = &actions[0] else { panic!("expected a start") };
        assert_eq!(plan.launch.network_interfaces.len(), 1);
        assert_eq!(plan.launch.metadata_seed.as_deref(), Some(METADATA_SEED));
    }

    #[test]
    fn a_container_boots_its_image_then_starts_its_workload() {
        let vm = container_vm();
        let mut waiting = context(vm.clone(), None);
        waiting.container_image = Some(container_image(ImagePhase::Pending));
        assert!(matches!(plan_vm(&waiting).as_slice(), [ComputeAction::ReportPhase { .. }]));
        waiting.container_image = Some(container_image(ImagePhase::Ready));
        let actions = plan_vm(&waiting);
        let ComputeAction::StartVm(plan) = &actions[0] else { panic!("expected a start") };
        assert_eq!(plan.launch.payload, Payload::ContainerTemplate);
        assert!(plan.launch.has_guest_agent);
        assert!(plan.root_disk.is_some());

        let mut booted = context(vm.clone(), Some(InstanceState::Running));
        booted.container_image = Some(container_image(ImagePhase::Ready));
        booted.workload = Some(WorkloadState::NotStarted);
        let ComputeAction::StartWorkload { workload, .. } = only(plan_vm(&booted)) else {
            panic!("expected a workload start")
        };
        assert_eq!(workload.arguments, vec!["sh"]);
        assert_eq!(workload.hostname, "web-1");
        booted.workload = Some(WorkloadState::Running);
        assert_eq!(plan_vm(&booted), vec![reported(&vm, VmPhase::Running)]);
    }

    #[test]
    fn a_container_whose_workload_exits_stays_down() {
        let vm = container_vm();
        let mut exited = context(vm.clone(), Some(InstanceState::Running));
        exited.workload = Some(WorkloadState::Exited { code: 3 });
        let actions = plan_vm(&exited);
        assert!(
            matches!(&actions[0], ComputeAction::ReportPhase { status, .. } if status.phase == VmPhase::Failed)
        );
        assert_eq!(actions[1], ComputeAction::StopVm(vm.metadata.id));
        let mut settled = vm.clone();
        settled.status.phase = VmPhase::Failed;
        settled.status.message = Some(String::from("the workload exited with code 3"));
        let mut down = context(settled, None);
        down.container_image = Some(container_image(ImagePhase::Ready));
        assert_eq!(plan_vm(&down), Vec::new());
    }

    #[test]
    fn an_exited_instance_is_cleaned_up_before_a_restart() {
        let vm = vm_on(Some("node-a"), VmPhase::Running);
        assert_eq!(
            plan_vm(&context(vm.clone(), Some(InstanceState::Exited))),
            vec![ComputeAction::StopVm(vm.metadata.id)]
        );
    }

    #[test]
    fn a_vm_process_failure_is_reported_and_kept_through_the_restart() {
        let vm = vm_on(Some("node-a"), VmPhase::Starting);
        let mut failed = context(vm.clone(), Some(InstanceState::Exited));
        if let Some(instance) = failed.instance.as_mut() {
            instance.failure = Some(String::from("the VM configuration is invalid"));
        }
        let actions = plan_vm(&failed);
        let ComputeAction::ReportPhase { status, .. } = &actions[0] else { panic!("expected a report") };
        assert_eq!(status.message.as_deref(), Some("the VM process failed: the VM configuration is invalid"));
        assert_eq!(actions[1], ComputeAction::StopVm(vm.metadata.id));
        let mut restarting = vm;
        restarting.status.message = status.message.clone();
        assert!(matches!(only(plan_vm(&context(restarting, None))), ComputeAction::StartVm(_)));
    }

    #[test]
    fn booting_vms_and_waiting_containers_are_in_transition() {
        let vm = vm_on(Some("node-a"), VmPhase::Running);
        assert!(is_in_transition(&context(vm.clone(), Some(InstanceState::Booting))));
        assert!(!is_in_transition(&context(vm, Some(InstanceState::Running))));
        let mut container = vm_on(Some("node-a"), VmPhase::Starting);
        container.specification = VmSpecification::container(name("busybox"));
        let mut waiting = context(container, Some(InstanceState::Running));
        assert!(is_in_transition(&waiting));
        waiting.workload = Some(WorkloadState::Running);
        assert!(!is_in_transition(&waiting));
    }

    #[test]
    fn instances_without_an_assigned_vm_are_stopped() {
        let kept = ObjectId::from_uuid(uuid::Uuid::new_v4());
        let orphan = ObjectId::from_uuid(uuid::Uuid::new_v4());
        let instances = [kept, orphan].map(|vm_id| RunningInstance::new(vm_id, InstanceState::Running));
        assert_eq!(plan_orphans(&instances, &BTreeSet::from([kept])), vec![ComputeAction::StopVm(orphan)]);
    }
}
