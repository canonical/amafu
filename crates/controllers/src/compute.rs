use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use amafu_domain::kinds::vm::{Vm, VmPhase, VmStatus};
use amafu_domain::{Change, NodeName, Object, ObjectId, ObjectReference};
use amafu_interfaces::objects::{apply, get, list, remove};
use amafu_interfaces::{
    ActionError, Consistency, Hypervisor, InstanceState, RunningInstance, Store, VmLaunchSpecification,
};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};

const STOP_GRACE_PERIOD: Duration = Duration::from_secs(10);

pub struct ComputeController {
    node: NodeName,
    store: Arc<dyn Store>,
    hypervisor: Arc<dyn Hypervisor>,
}

impl ComputeController {
    #[must_use]
    pub fn new(node: NodeName, store: Arc<dyn Store>, hypervisor: Arc<dyn Hypervisor>) -> Self {
        Self { node, store, hypervisor }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ComputeKey {
    Vm(ObjectReference),
    Orphans,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ComputeObservation {
    Vm { node: NodeName, vm: Option<Box<Object<Vm>>>, instance: Option<RunningInstance> },
    Orphans { instances: Vec<RunningInstance>, assigned: BTreeSet<ObjectId> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComputeAction {
    StartVm(VmLaunchSpecification),
    StopVm(ObjectId),
    ReportPhase { observed_generation: u64, status: VmStatus },
    Remove,
}

#[must_use]
pub fn launch_specification(vm: &Object<Vm>) -> VmLaunchSpecification {
    VmLaunchSpecification {
        vm_id: vm.metadata.id,
        cpu_count: vm.specification.cpu_count,
        memory_mebibytes: vm.specification.memory_mebibytes,
        image: vm.specification.image.clone(),
    }
}

fn report(vm: &Object<Vm>, node: &NodeName, phase: VmPhase) -> Option<ComputeAction> {
    let status = VmStatus { phase, node: Some(node.clone()), message: None };
    if vm.status == status && vm.is_status_current() {
        return None;
    }
    Some(ComputeAction::ReportPhase { observed_generation: vm.metadata.generation, status })
}

#[must_use]
pub fn plan_compute(observation: &ComputeObservation) -> Vec<ComputeAction> {
    match observation {
        ComputeObservation::Vm { node, vm, instance } => plan_vm(node, vm.as_deref(), instance.as_ref()),
        ComputeObservation::Orphans { instances, assigned } => instances
            .iter()
            .filter(|instance| !assigned.contains(&instance.vm_id))
            .map(|instance| ComputeAction::StopVm(instance.vm_id))
            .collect(),
    }
}

fn plan_vm(
    node: &NodeName,
    vm: Option<&Object<Vm>>,
    instance: Option<&RunningInstance>,
) -> Vec<ComputeAction> {
    let Some(vm) = vm else {
        return instance.map(|instance| ComputeAction::StopVm(instance.vm_id)).into_iter().collect();
    };
    if vm.status.node.as_ref() != Some(node) {
        return instance.map(|instance| ComputeAction::StopVm(instance.vm_id)).into_iter().collect();
    }
    let should_run = vm.specification.should_run() && !vm.metadata.is_deleting;
    let state = instance.map(|instance| instance.state);
    let has_failed_at_this_generation = vm.status.phase == VmPhase::Failed && vm.is_status_current();
    match (should_run, state) {
        (true, None) if has_failed_at_this_generation => Vec::new(),
        (true, None) => {
            let mut actions = vec![ComputeAction::StartVm(launch_specification(vm))];
            actions.extend(report(vm, node, VmPhase::Starting));
            actions
        }
        (true, Some(InstanceState::Booting)) => report(vm, node, VmPhase::Starting).into_iter().collect(),
        (true, Some(InstanceState::Running)) => report(vm, node, VmPhase::Running).into_iter().collect(),
        (_, Some(InstanceState::Exited)) => vec![ComputeAction::StopVm(vm.metadata.id)],
        (false, Some(InstanceState::Booting | InstanceState::Running)) => {
            let mut actions: Vec<ComputeAction> = report(vm, node, VmPhase::Stopping).into_iter().collect();
            actions.push(ComputeAction::StopVm(vm.metadata.id));
            actions
        }
        (false, None) if vm.metadata.is_deleting => vec![ComputeAction::Remove],
        (false, None) => report(vm, node, VmPhase::Stopped).into_iter().collect(),
    }
}

impl ComputeController {
    async fn report_failure(&self, key: &ComputeKey, reason: &str) -> Result<(), ActionError> {
        let ComputeKey::Vm(reference) = key else {
            return Err(ActionError::fatal("a failure report needs a VM key"));
        };
        let Some(vm) = get::<Vm>(self.store.as_ref(), reference, Consistency::Linearizable).await? else {
            return Ok(());
        };
        let status = VmStatus {
            phase: VmPhase::Failed,
            node: Some(self.node.clone()),
            message: Some(reason.to_owned()),
        };
        let change = Change::UpdateStatus { observed_generation: vm.metadata.generation, status };
        apply::<Vm>(self.store.as_ref(), reference, change).await?;
        Ok(())
    }

    async fn instances(&self) -> Result<Vec<RunningInstance>, ActionError> {
        self.hypervisor.running().await
    }

    async fn vms_assigned_here(&self) -> Result<Vec<Object<Vm>>, ActionError> {
        let vms = list::<Vm>(self.store.as_ref(), None, Consistency::Linearizable).await?;
        Ok(vms.into_iter().filter(|vm| vm.status.node.as_ref() == Some(&self.node)).collect())
    }
}

impl Controller for ComputeController {
    type Key = ComputeKey;
    type Observation = ComputeObservation;
    type Action = ComputeAction;

    const NAME: &'static str = "compute";
    const WATCHES: &'static [WatchedKind<ComputeKey>] = &[WatchedKind {
        kind: "Vm",
        mapping: KeyMapping::ObjectKey(|reference| Some(ComputeKey::Vm(reference.clone()))),
    }];

    async fn discover_keys(&self) -> Result<Vec<ComputeKey>, ActionError> {
        let mut keys: Vec<ComputeKey> =
            self.vms_assigned_here().await?.iter().map(|vm| ComputeKey::Vm(vm.reference())).collect();
        keys.push(ComputeKey::Orphans);
        Ok(keys)
    }

    async fn observe(&self, key: &ComputeKey) -> Result<ComputeObservation, ActionError> {
        match key {
            ComputeKey::Vm(reference) => {
                let vm =
                    get::<Vm>(self.store.as_ref(), reference, Consistency::Linearizable).await?.map(Box::new);
                let instance = match &vm {
                    Some(vm) => {
                        self.instances().await?.into_iter().find(|instance| instance.vm_id == vm.metadata.id)
                    }
                    None => None,
                };
                Ok(ComputeObservation::Vm { node: self.node.clone(), vm, instance })
            }
            ComputeKey::Orphans => {
                let instances = self.instances().await?;
                let assigned = self.vms_assigned_here().await?.iter().map(|vm| vm.metadata.id).collect();
                Ok(ComputeObservation::Orphans { instances, assigned })
            }
        }
    }

    fn plan(&self, observation: &ComputeObservation) -> Vec<ComputeAction> {
        plan_compute(observation)
    }

    async fn apply(&self, key: &ComputeKey, action: ComputeAction) -> Result<(), ActionError> {
        match action {
            ComputeAction::StartVm(launch) => match self.hypervisor.start(&launch).await {
                Err(ActionError::Invalid(reason)) => {
                    self.report_failure(key, &reason).await?;
                    Err(ActionError::Invalid(reason))
                }
                outcome => outcome,
            },
            ComputeAction::StopVm(vm_id) => self.hypervisor.stop(&vm_id, STOP_GRACE_PERIOD).await,
            ComputeAction::ReportPhase { observed_generation, status } => {
                let ComputeKey::Vm(reference) = key else {
                    return Err(ActionError::fatal("a phase report needs a VM key"));
                };
                apply::<Vm>(
                    self.store.as_ref(),
                    reference,
                    Change::UpdateStatus { observed_generation, status },
                )
                .await?;
                Ok(())
            }
            ComputeAction::Remove => {
                let ComputeKey::Vm(reference) = key else {
                    return Err(ActionError::fatal("removal needs a VM key"));
                };
                Ok(remove::<Vm>(self.store.as_ref(), reference).await?)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use amafu_domain::kinds::vm::{PowerState, VmSpecification};
    use amafu_domain::{Metadata, ObjectName, ProjectName};

    use super::*;

    fn here() -> NodeName {
        NodeName::new("node-a").unwrap()
    }

    fn vm_on(node: Option<&str>, phase: VmPhase) -> Object<Vm> {
        Object {
            metadata: Metadata {
                name: ObjectName::new("web-1").unwrap(),
                project: Some(ProjectName::new("project-a").unwrap()),
                id: ObjectId::from_uuid(uuid::Uuid::new_v4()),
                version: 1,
                generation: 1,
                observed_generation: 1,
                labels: BTreeMap::new(),
                is_deleting: false,
            },
            specification: VmSpecification::small(),
            status: VmStatus { phase, node: node.map(|node| NodeName::new(node).unwrap()), message: None },
        }
    }

    fn instance(vm: &Object<Vm>, state: InstanceState) -> RunningInstance {
        RunningInstance { vm_id: vm.metadata.id, state }
    }

    fn plan(vm: Option<&Object<Vm>>, instance: Option<RunningInstance>) -> Vec<ComputeAction> {
        plan_compute(&ComputeObservation::Vm { node: here(), vm: vm.cloned().map(Box::new), instance })
    }

    fn reported(vm: &Object<Vm>, phase: VmPhase) -> ComputeAction {
        ComputeAction::ReportPhase {
            observed_generation: vm.metadata.generation,
            status: VmStatus { phase, node: Some(here()), message: None },
        }
    }

    #[test]
    fn a_scheduled_vm_is_started_and_reported_starting() {
        let vm = vm_on(Some("node-a"), VmPhase::Scheduled);
        assert_eq!(
            plan(Some(&vm), None),
            vec![ComputeAction::StartVm(launch_specification(&vm)), reported(&vm, VmPhase::Starting)]
        );
    }

    #[test]
    fn a_booting_instance_is_reported_once() {
        let vm = vm_on(Some("node-a"), VmPhase::Scheduled);
        assert_eq!(
            plan(Some(&vm), Some(instance(&vm, InstanceState::Booting))),
            vec![reported(&vm, VmPhase::Starting)]
        );
        let starting = vm_on(Some("node-a"), VmPhase::Starting);
        assert_eq!(plan(Some(&starting), Some(instance(&starting, InstanceState::Booting))), Vec::new());
    }

    #[test]
    fn a_running_instance_is_reported_running() {
        let vm = vm_on(Some("node-a"), VmPhase::Starting);
        assert_eq!(
            plan(Some(&vm), Some(instance(&vm, InstanceState::Running))),
            vec![reported(&vm, VmPhase::Running)]
        );
    }

    #[test]
    fn a_steady_running_vm_needs_nothing() {
        let vm = vm_on(Some("node-a"), VmPhase::Running);
        assert_eq!(plan(Some(&vm), Some(instance(&vm, InstanceState::Running))), Vec::new());
    }

    #[test]
    fn a_new_generation_is_acknowledged_even_without_a_phase_change() {
        let mut vm = vm_on(Some("node-a"), VmPhase::Running);
        vm.metadata.generation = 2;
        assert_eq!(
            plan(Some(&vm), Some(instance(&vm, InstanceState::Running))),
            vec![reported(&vm, VmPhase::Running)]
        );
    }

    #[test]
    fn a_vm_placed_elsewhere_is_never_run_here() {
        let vm = vm_on(Some("node-b"), VmPhase::Scheduled);
        assert_eq!(plan(Some(&vm), None), Vec::new());
        assert_eq!(
            plan(Some(&vm), Some(instance(&vm, InstanceState::Running))),
            vec![ComputeAction::StopVm(vm.metadata.id)]
        );
    }

    #[test]
    fn a_vm_powered_off_is_reported_stopping_then_stopped() {
        let mut vm = vm_on(Some("node-a"), VmPhase::Running);
        vm.specification.power_state = PowerState::Stopped;
        assert_eq!(
            plan(Some(&vm), Some(instance(&vm, InstanceState::Running))),
            vec![reported(&vm, VmPhase::Stopping), ComputeAction::StopVm(vm.metadata.id)]
        );
        assert_eq!(plan(Some(&vm), None), vec![reported(&vm, VmPhase::Stopped)]);
    }

    #[test]
    fn a_deleted_vm_is_stopped_then_removed() {
        let mut vm = vm_on(Some("node-a"), VmPhase::Stopping);
        vm.metadata.is_deleting = true;
        assert_eq!(
            plan(Some(&vm), Some(instance(&vm, InstanceState::Running))),
            vec![ComputeAction::StopVm(vm.metadata.id)]
        );
        assert_eq!(plan(Some(&vm), None), vec![ComputeAction::Remove]);
    }

    #[test]
    fn an_exited_instance_is_cleaned_up_before_a_restart() {
        let vm = vm_on(Some("node-a"), VmPhase::Running);
        assert_eq!(
            plan(Some(&vm), Some(instance(&vm, InstanceState::Exited))),
            vec![ComputeAction::StopVm(vm.metadata.id)]
        );
    }

    #[test]
    fn a_vm_that_failed_waits_for_a_new_specification() {
        let mut vm = vm_on(Some("node-a"), VmPhase::Failed);
        assert_eq!(plan(Some(&vm), None), Vec::new());
        vm.metadata.generation = 2;
        assert_eq!(
            plan(Some(&vm), None),
            vec![ComputeAction::StartVm(launch_specification(&vm)), reported(&vm, VmPhase::Starting)]
        );
    }

    #[test]
    fn instances_without_an_assigned_vm_are_stopped() {
        let kept = vm_on(Some("node-a"), VmPhase::Running);
        let orphan = vm_on(Some("node-a"), VmPhase::Running);
        let observation = ComputeObservation::Orphans {
            instances: vec![
                instance(&kept, InstanceState::Running),
                instance(&orphan, InstanceState::Running),
            ],
            assigned: BTreeSet::from([kept.metadata.id]),
        };
        assert_eq!(plan_compute(&observation), vec![ComputeAction::StopVm(orphan.metadata.id)]);
    }
}
