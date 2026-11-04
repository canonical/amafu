use std::sync::Arc;

use amafu_domain::kinds::operation::StepSpecification;
use amafu_interfaces::{ActionError, Identifiers, Storage};
use amafu_reconciler::{Step, StepRegistry};

use crate::operations::migration::{
    AnnounceAddresses, CleanUpSource, CopyMemory, Cordon, MigrateVm, PrepareDestination, SwitchOver,
};
use crate::operations::recovery::{MarkNodeLost, ReleaseAssignments, RevokeNodeOnStorage, WaitOutWatchdog};

pub struct OperationSteps {
    storage: Arc<dyn Storage>,
    identifiers: Arc<dyn Identifiers>,
}

impl OperationSteps {
    #[must_use]
    pub fn new(storage: Arc<dyn Storage>, identifiers: Arc<dyn Identifiers>) -> Self {
        Self { storage, identifiers }
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
            StepSpecification::PrepareDestination { vm, destination } => {
                Box::new(PrepareDestination { vm: vm.clone(), destination: destination.clone() })
            }
            StepSpecification::CopyMemory { vm } => Box::new(CopyMemory { vm: vm.clone() }),
            StepSpecification::SwitchOver { vm } => Box::new(SwitchOver { vm: vm.clone() }),
            StepSpecification::AnnounceAddresses { vm } => Box::new(AnnounceAddresses { vm: vm.clone() }),
            StepSpecification::CleanUpSource { vm } => Box::new(CleanUpSource { vm: vm.clone() }),
            StepSpecification::Cordon { node } => Box::new(Cordon { node: node.clone() }),
            StepSpecification::MigrateVm { vm } => {
                Box::new(MigrateVm { vm: vm.clone(), identifiers: Arc::clone(&self.identifiers) })
            }
        })
    }
}
