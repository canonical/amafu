use std::sync::Arc;

use amafu_domain::kinds::operation::StepSpecification;
use amafu_interfaces::{ActionError, Identifiers, Storage, TrustedImageKeys};
use amafu_reconciler::{Step, StepRegistry};

use crate::operations::migration::{
    AnnounceAddresses, CleanUpSource, CopyMemory, Cordon, MigrateVm, PrepareDestination, SwitchOver,
};
use crate::operations::recovery::{MarkNodeLost, ReleaseAssignments, RevokeNodeOnStorage, WaitOutWatchdog};
use crate::operations::upgrade::{
    AwaitHealthy, Evacuate, RebootIntoSlot, SetClusterImage, Soak, Uncordon, VerifyImage, WriteInactiveSlot,
};
use crate::scheduling::SchedulingSettings;

pub struct OperationSteps {
    storage: Arc<dyn Storage>,
    identifiers: Arc<dyn Identifiers>,
    scheduling: SchedulingSettings,
    image_keys: Arc<TrustedImageKeys>,
}

impl OperationSteps {
    #[must_use]
    pub fn new(storage: Arc<dyn Storage>, identifiers: Arc<dyn Identifiers>) -> Self {
        Self {
            storage,
            identifiers,
            scheduling: SchedulingSettings::default(),
            image_keys: Arc::new(TrustedImageKeys::default()),
        }
    }

    #[must_use]
    pub fn scheduling(mut self, scheduling: SchedulingSettings) -> Self {
        self.scheduling = scheduling;
        self
    }

    #[must_use]
    pub fn image_keys(mut self, image_keys: TrustedImageKeys) -> Self {
        self.image_keys = Arc::new(image_keys);
        self
    }
}

impl StepRegistry for OperationSteps {
    fn build(&self, specification: &StepSpecification) -> Result<Box<dyn Step>, ActionError> {
        Ok(match specification {
            StepSpecification::MarkNodeLost { node } => Box::new(MarkNodeLost { node: node.clone() }),
            StepSpecification::RevokeNodeOnStorage { node } => {
                Box::new(RevokeNodeOnStorage { node: node.clone(), storage: Arc::clone(&self.storage) })
            }
            StepSpecification::WaitOutWatchdog { node } => Box::new(WaitOutWatchdog { node: node.clone() }),
            StepSpecification::ReleaseAssignments { node } => {
                Box::new(ReleaseAssignments { node: node.clone(), storage: Arc::clone(&self.storage) })
            }
            StepSpecification::PrepareDestination { vm, destination } => Box::new(PrepareDestination {
                vm: vm.clone(),
                destination: destination.clone(),
                scheduling: self.scheduling,
            }),
            StepSpecification::CopyMemory { vm } => Box::new(CopyMemory { vm: vm.clone() }),
            StepSpecification::SwitchOver { vm } => Box::new(SwitchOver { vm: vm.clone() }),
            StepSpecification::AnnounceAddresses { vm } => Box::new(AnnounceAddresses { vm: vm.clone() }),
            StepSpecification::CleanUpSource { vm } => Box::new(CleanUpSource { vm: vm.clone() }),
            StepSpecification::Cordon { node } => Box::new(Cordon { node: node.clone() }),
            StepSpecification::MigrateVm { vm } => {
                Box::new(MigrateVm { vm: vm.clone(), identifiers: Arc::clone(&self.identifiers) })
            }
            StepSpecification::VerifyImage => Box::new(VerifyImage { keys: Arc::clone(&self.image_keys) }),
            StepSpecification::SetClusterImage => {
                Box::new(SetClusterImage { identifiers: Arc::clone(&self.identifiers) })
            }
            StepSpecification::Evacuate { node } => {
                Box::new(Evacuate { node: node.clone(), identifiers: Arc::clone(&self.identifiers) })
            }
            StepSpecification::WriteInactiveSlot { node } => {
                Box::new(WriteInactiveSlot { node: node.clone() })
            }
            StepSpecification::RebootIntoSlot { node } => Box::new(RebootIntoSlot { node: node.clone() }),
            StepSpecification::AwaitHealthy { node } => Box::new(AwaitHealthy { node: node.clone() }),
            StepSpecification::Uncordon { node } => Box::new(Uncordon { node: node.clone() }),
            StepSpecification::Soak => Box::new(Soak),
            StepSpecification::DeployMachine { .. } => {
                return Err(ActionError::fatal("machines are deployed by the installer, not by the cluster"));
            }
        })
    }
}
