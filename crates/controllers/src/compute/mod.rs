pub mod plan;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use amafu_domain::kinds::image::Image;
use amafu_domain::kinds::storage_backend::StorageBackend;
use amafu_domain::kinds::vm::{Vm, VmPhase, VmStatus};
use amafu_domain::kinds::volume::Volume;
use amafu_domain::{Change, NodeName, Object, ObjectId, ObjectReference};
use amafu_interfaces::objects::{apply, get, list, remove};
use amafu_interfaces::{
    ActionError, AttachTarget, Clock, Consistency, Datapath, GuestAgents, Hypervisor, ImageStore,
    InstanceState, LaunchDisk, NodeLeaseState, RunningInstance, Storage, Store,
};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};
pub use plan::{ComputeAction, ObservedImage, ObservedVolume, StartPlan, VmContext, VolumeAttachment};

use crate::references::{cluster_reference, project_reference};

const STOP_GRACE_PERIOD: Duration = Duration::from_secs(10);

const TRANSITION_REVISIT: std::time::Duration = std::time::Duration::from_millis(100);

#[derive(Clone)]
pub struct ComputeServices {
    pub store: Arc<dyn Store>,
    pub hypervisor: Arc<dyn Hypervisor>,
    pub storage: Arc<dyn Storage>,
    pub images: Arc<dyn ImageStore>,
    pub datapath: Arc<dyn Datapath>,
    pub agents: Arc<dyn GuestAgents>,
    pub lease: NodeLeaseState,
    pub clock: Arc<dyn Clock>,
}

pub struct ComputeController {
    node: NodeName,
    services: ComputeServices,
}

impl ComputeController {
    fn holds_lease(&self) -> bool {
        self.services.lease.epoch_valid_at(self.services.clock.now()).is_some()
    }

    #[must_use]
    pub fn new(node: NodeName, services: ComputeServices) -> Self {
        Self { node, services }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ComputeKey {
    Vm(ObjectReference),
    Orphans,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ComputeObservation {
    Hosted(Box<VmContext>),
    Elsewhere { instance: Option<RunningInstance> },
    Orphans { instances: Vec<RunningInstance>, assigned: BTreeSet<ObjectId> },
}

#[must_use]
pub fn plan_compute(observation: &ComputeObservation) -> Vec<ComputeAction> {
    match observation {
        ComputeObservation::Hosted(context) => plan::plan_vm(context),
        ComputeObservation::Elsewhere { instance } => plan::plan_stray(instance.as_ref()),
        ComputeObservation::Orphans { instances, assigned } => plan::plan_orphans(instances, assigned),
    }
}

impl ComputeController {
    fn store(&self) -> &dyn Store {
        self.services.store.as_ref()
    }

    async fn instance_of(&self, vm_id: ObjectId) -> Result<Option<RunningInstance>, ActionError> {
        Ok(self.services.hypervisor.running().await?.into_iter().find(|instance| instance.vm_id == vm_id))
    }

    async fn vms_assigned_here(&self) -> Result<Vec<Object<Vm>>, ActionError> {
        let vms = list::<Vm>(self.store(), None, Consistency::Linearizable).await?;
        Ok(vms.into_iter().filter(|vm| vm.status.node.as_ref() == Some(&self.node)).collect())
    }

    async fn observe_volumes(&self, vm: &Object<Vm>) -> Result<Vec<ObservedVolume>, ActionError> {
        let mut observed = Vec::new();
        for name in &vm.specification.volumes {
            let volume = get::<Volume>(
                self.store(),
                &project_reference::<Volume>(vm, name),
                Consistency::Linearizable,
            )
            .await?;
            let backend = match &volume {
                Some(volume) => get::<StorageBackend>(
                    self.store(),
                    &cluster_reference::<StorageBackend>(&volume.specification.backend),
                    Consistency::Linearizable,
                )
                .await?
                .map(|backend| backend.specification.provider),
                None => None,
            };
            observed.push(ObservedVolume { name: name.clone(), volume, backend });
        }
        Ok(observed)
    }

    async fn observe_container_image(&self, vm: &Object<Vm>) -> Result<Option<ObservedImage>, ActionError> {
        let Some(name) = vm.specification.container_image() else {
            return Ok(None);
        };
        let image =
            get::<Image>(self.store(), &project_reference::<Image>(vm, name), Consistency::Linearizable)
                .await?;
        let cached = match &image {
            Some(image) => self.services.images.cached(&image.metadata.id).await?,
            None => None,
        };
        Ok(Some(ObservedImage { image, cached }))
    }

    async fn observe_hosted(&self, vm: Object<Vm>) -> Result<VmContext, ActionError> {
        let instance = self.instance_of(vm.metadata.id).await?;
        let is_booted_container = vm.specification.is_container()
            && instance.as_ref().is_some_and(|instance| instance.state == InstanceState::Running);
        let workload = if is_booted_container {
            self.services.agents.workload(&vm.metadata.id).await.ok()
        } else {
            None
        };
        Ok(VmContext {
            node: self.node.clone(),
            volumes: self.observe_volumes(&vm).await?,
            container_image: self.observe_container_image(&vm).await?,
            realized_ports: self.services.datapath.current().await?.ports,
            instance,
            workload,
            vm,
        })
    }

    async fn target_of(&self, attachment: &VolumeAttachment) -> Result<AttachTarget, ActionError> {
        let provider = self.services.storage.provider(&attachment.backend)?;
        provider.export_to_node(&attachment.provisioned, &self.node).await
    }

    async fn start(&self, plan: StartPlan) -> Result<(), ActionError> {
        let mut launch = plan.launch;
        if let Some(root) = plan.root_disk {
            launch.disks.push(LaunchDisk { path: root, is_read_only: true });
        }
        for attachment in &plan.volumes {
            let target = self.target_of(attachment).await?;
            let path = self.services.storage.attacher(target.protocol())?.attach(&target).await?;
            launch.disks.push(LaunchDisk { path, is_read_only: false });
        }
        self.services.hypervisor.start(&launch).await
    }

    async fn release(&self, attachments: &[VolumeAttachment]) -> Result<(), ActionError> {
        for attachment in attachments {
            let target = self.target_of(attachment).await?;
            self.services.storage.attacher(target.protocol())?.detach(&target).await?;
        }
        Ok(())
    }

    async fn report_failure(&self, reference: &ObjectReference, reason: &str) -> Result<(), ActionError> {
        let Some(vm) = get::<Vm>(self.store(), reference, Consistency::Linearizable).await? else {
            return Ok(());
        };
        let status = VmStatus {
            phase: VmPhase::Failed,
            node: Some(self.node.clone()),
            interfaces: vm.status.interfaces.clone(),
            message: Some(reason.to_owned()),
            migration: vm.status.migration.clone(),
        };
        apply::<Vm>(
            self.store(),
            reference,
            Change::UpdateStatus { observed_generation: vm.metadata.generation, status },
        )
        .await?;
        Ok(())
    }
}

fn vm_reference(key: &ComputeKey) -> Result<&ObjectReference, ActionError> {
    match key {
        ComputeKey::Vm(reference) => Ok(reference),
        ComputeKey::Orphans => Err(ActionError::fatal("this action needs a VM key")),
    }
}

impl Controller for ComputeController {
    type Key = ComputeKey;
    type Observation = ComputeObservation;
    type Action = ComputeAction;

    const NAME: &'static str = "compute";
    const WATCHES: &'static [WatchedKind<ComputeKey>] = &[
        WatchedKind {
            kind: "Vm",
            mapping: KeyMapping::ObjectKey(|reference| Some(ComputeKey::Vm(reference.clone()))),
        },
        WatchedKind { kind: "Volume", mapping: KeyMapping::EveryKey },
        WatchedKind { kind: "Image", mapping: KeyMapping::EveryKey },
    ];

    async fn discover_keys(&self) -> Result<Vec<ComputeKey>, ActionError> {
        let mut keys: Vec<ComputeKey> =
            self.vms_assigned_here().await?.iter().map(|vm| ComputeKey::Vm(vm.reference())).collect();
        keys.push(ComputeKey::Orphans);
        Ok(keys)
    }

    async fn observe(&self, key: &ComputeKey) -> Result<ComputeObservation, ActionError> {
        match key {
            ComputeKey::Vm(reference) => {
                match get::<Vm>(self.store(), reference, Consistency::Linearizable).await? {
                    Some(vm) if vm.status.node.as_ref() == Some(&self.node) => {
                        Ok(ComputeObservation::Hosted(Box::new(self.observe_hosted(vm).await?)))
                    }
                    Some(vm) => Ok(ComputeObservation::Elsewhere {
                        instance: self.instance_of(vm.metadata.id).await?,
                    }),
                    None => Ok(ComputeObservation::Elsewhere { instance: None }),
                }
            }
            ComputeKey::Orphans => {
                let instances = self.services.hypervisor.running().await?;
                let assigned = self.vms_assigned_here().await?.iter().map(|vm| vm.metadata.id).collect();
                Ok(ComputeObservation::Orphans { instances, assigned })
            }
        }
    }

    fn plan(&self, observation: &ComputeObservation) -> Vec<ComputeAction> {
        plan_compute(observation)
    }

    fn revisit_after(&self, observation: &ComputeObservation) -> Option<std::time::Duration> {
        let ComputeObservation::Hosted(context) = observation else {
            return None;
        };
        plan::is_in_transition(context).then_some(TRANSITION_REVISIT)
    }

    async fn apply(&self, key: &ComputeKey, action: ComputeAction) -> Result<(), ActionError> {
        match action {
            ComputeAction::StartVm(_) if !self.holds_lease() => {
                Err(ActionError::retryable("the node holds no valid lease, so it starts no VM"))
            }
            ComputeAction::StartVm(plan) => match self.start(*plan).await {
                Err(ActionError::Invalid(reason)) => {
                    self.report_failure(vm_reference(key)?, &reason).await?;
                    Err(ActionError::Invalid(reason))
                }
                outcome => outcome,
            },
            ComputeAction::StartWorkload { vm_id, workload } => {
                self.services.agents.start_workload(&vm_id, &workload).await
            }
            ComputeAction::StopVm(vm_id) => self.services.hypervisor.stop(&vm_id, STOP_GRACE_PERIOD).await,
            ComputeAction::ReleaseVolumes(attachments) => self.release(&attachments).await,
            ComputeAction::ReportPhase { observed_generation, status } => {
                apply::<Vm>(
                    self.store(),
                    vm_reference(key)?,
                    Change::UpdateStatus { observed_generation, status },
                )
                .await?;
                Ok(())
            }
            ComputeAction::Remove => Ok(remove::<Vm>(self.store(), vm_reference(key)?).await?),
        }
    }
}
