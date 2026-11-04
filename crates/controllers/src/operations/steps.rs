use std::sync::Arc;

use amafu_domain::kinds::operation::StepSpecification;
use amafu_interfaces::{ActionError, Storage};
use amafu_reconciler::{Step, StepRegistry};

use crate::operations::recovery::{MarkNodeLost, ReleaseAssignments, RevokeNodeOnStorage, WaitOutWatchdog};

pub struct OperationSteps {
    storage: Arc<dyn Storage>,
}

impl OperationSteps {
    #[must_use]
    pub fn new(storage: Arc<dyn Storage>) -> Self {
        Self { storage }
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
            other => {
                return Err(ActionError::invalid(format!("the {} step has no implementation", other.name())));
            }
        })
    }
}
