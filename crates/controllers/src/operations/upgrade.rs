use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use amafu_domain::kinds::cluster_version::{
    ClusterVersion, ClusterVersionSpecification, cluster_version_name,
};
use amafu_domain::kinds::node::{Node, NodeImageRequest, NodeSpecification};
use amafu_domain::kinds::operation::{
    Operation, OperationKind, OperationOutcome, OperationSpecification, VmReference,
};
use amafu_domain::kinds::vm::{Vm, VmPhase};
use amafu_domain::node_image::NodeImage;
use amafu_domain::upgrade::DEFAULT_HEALTH_DEADLINE_SECONDS;
use amafu_domain::{Change, NodeName, Object, ObjectId, ObjectReference};
use amafu_interfaces::objects::{apply, get, list};
use amafu_interfaces::{ActionError, Consistency, Identifiers, Store, TrustedImageKeys};
use amafu_reconciler::{OperationContext, Step};

use crate::operations::migration::child_name;
use crate::operations::node_specification::{change_node_specification, node_object, node_reference};

pub fn planned_image<'context>(
    context: &'context OperationContext<'_>,
) -> Result<&'context NodeImage, ActionError> {
    context
        .operation
        .specification
        .image()
        .ok_or_else(|| ActionError::fatal("the operation carries no image"))
}

fn step_age(context: &OperationContext<'_>) -> Duration {
    context
        .operation
        .status
        .current_step
        .as_ref()
        .and_then(|progress| Duration::try_from(context.clock.now() - progress.started_at).ok())
        .unwrap_or_default()
}

fn cluster_version_reference() -> ObjectReference {
    ObjectReference::of::<ClusterVersion>(None, cluster_version_name())
}

pub async fn cluster_version(store: &dyn Store) -> Result<Option<Object<ClusterVersion>>, ActionError> {
    Ok(get::<ClusterVersion>(store, &cluster_version_reference(), Consistency::Linearizable).await?)
}

fn fallen_back(node: &Object<Node>, image: &NodeImage) -> Result<(), ActionError> {
    if node.status.boot.as_ref().is_some_and(|boot| boot.fell_back_from(image)) {
        return Err(ActionError::invalid(format!(
            "{} could not boot {} and went back to its previous image",
            node.metadata.name,
            image.identity()
        )));
    }
    Ok(())
}

pub struct VerifyImage {
    pub keys: Arc<TrustedImageKeys>,
}

#[async_trait::async_trait]
impl Step for VerifyImage {
    async fn is_complete(&self, context: &OperationContext<'_>) -> Result<bool, ActionError> {
        let image = planned_image(context)?;
        self.keys.verify(image).map_err(ActionError::invalid)?;
        if let Some(cluster) = cluster_version(context.store).await?
            && !cluster.specification.admits(image)
        {
            return Err(ActionError::invalid(format!(
                "the cluster is finalized at feature level {}, which {} does not support",
                cluster.specification.finalized_feature_level,
                image.identity()
            )));
        }
        Ok(true)
    }

    async fn run(&self, _context: &OperationContext<'_>) -> Result<(), ActionError> {
        Ok(())
    }
}

pub struct SetClusterImage {
    pub identifiers: Arc<dyn Identifiers>,
}

#[async_trait::async_trait]
impl Step for SetClusterImage {
    async fn is_complete(&self, context: &OperationContext<'_>) -> Result<bool, ActionError> {
        let image = planned_image(context)?;
        Ok(cluster_version(context.store).await?.is_some_and(|cluster| cluster.specification.image == *image))
    }

    async fn run(&self, context: &OperationContext<'_>) -> Result<(), ActionError> {
        let image = planned_image(context)?.clone();
        let reference = cluster_version_reference();
        let change = match cluster_version(context.store).await? {
            None => Change::Create {
                id: ObjectId::from_uuid(self.identifiers.generate()),
                name: reference.name.clone(),
                project: None,
                labels: BTreeMap::new(),
                specification: ClusterVersionSpecification::installed(image),
            },
            Some(cluster) if !cluster.specification.admits(&image) => {
                return Err(ActionError::invalid(format!(
                    "the cluster was finalized at feature level {}, so it cannot go back to {}",
                    cluster.specification.finalized_feature_level,
                    image.identity()
                )));
            }
            Some(cluster) => Change::UpdateSpecification {
                expected_generation: Some(cluster.metadata.generation),
                specification: cluster.specification.moved_to(image),
            },
        };
        apply::<ClusterVersion>(context.store, &reference, change).await?;
        Ok(())
    }
}

pub struct Evacuate {
    pub node: NodeName,
    pub identifiers: Arc<dyn Identifiers>,
}

impl Evacuate {
    fn is_running_here(&self, vm: &Object<Vm>) -> bool {
        let runs_here = vm.status.node.as_ref() == Some(&self.node) && vm.status.phase == VmPhase::Running;
        let leaves_here = vm.status.active_migration().is_some_and(|migration| migration.source == self.node);
        runs_here || leaves_here
    }

    async fn running_here(&self, store: &dyn Store) -> Result<Vec<Object<Vm>>, ActionError> {
        Ok(list::<Vm>(store, None, Consistency::Linearizable)
            .await?
            .into_iter()
            .filter(|vm| self.is_running_here(vm))
            .collect())
    }

    fn child_reference(&self, context: &OperationContext<'_>) -> Result<ObjectReference, ActionError> {
        Ok(ObjectReference::of::<Operation>(None, child_name(context, "evacuate", self.node.as_str())?))
    }
}

#[async_trait::async_trait]
impl Step for Evacuate {
    async fn is_complete(&self, context: &OperationContext<'_>) -> Result<bool, ActionError> {
        let node = node_object(context.store, &self.node).await?;
        Ok(node.specification.is_cordoned && self.running_here(context.store).await?.is_empty())
    }

    async fn run(&self, context: &OperationContext<'_>) -> Result<(), ActionError> {
        let reference = self.child_reference(context)?;
        if let Some(child) = get::<Operation>(context.store, &reference, Consistency::Linearizable).await? {
            let failure = match &child.status.outcome {
                None => return Ok(()),
                Some(OperationOutcome::Succeeded) => None,
                Some(OperationOutcome::Cancelled) => Some(String::from("the evacuation was cancelled")),
                Some(OperationOutcome::Failed { reason }) => Some(reason.clone()),
            };
            apply::<Operation>(context.store, &reference, Change::Delete).await?;
            return match failure {
                Some(reason) => {
                    Err(ActionError::retryable(format!("evacuating {} failed: {reason}", self.node)))
                }
                None => Ok(()),
            };
        }
        let vms: Vec<VmReference> = self
            .running_here(context.store)
            .await?
            .into_iter()
            .filter(|vm| !vm.status.is_migrating())
            .filter_map(|vm| Some(VmReference { project: vm.metadata.project?, name: vm.metadata.name }))
            .collect();
        apply::<Operation>(
            context.store,
            &reference,
            Change::Create {
                id: ObjectId::from_uuid(self.identifiers.generate()),
                name: reference.name.clone(),
                project: None,
                labels: BTreeMap::new(),
                specification: OperationSpecification::evacuation(self.node.clone(), vms),
            },
        )
        .await?;
        Ok(())
    }
}

async fn request_image(
    store: &dyn Store,
    node: &NodeName,
    image: &NodeImage,
    should_boot: bool,
) -> Result<(), ActionError> {
    change_node_specification(store, node, |current| {
        let should_boot = should_boot
            || current.image.as_ref().is_some_and(|request| request.image == *image && request.should_boot);
        NodeSpecification {
            image: Some(NodeImageRequest { image: image.clone(), should_boot }),
            ..current.clone()
        }
    })
    .await
}

pub struct WriteInactiveSlot {
    pub node: NodeName,
}

#[async_trait::async_trait]
impl Step for WriteInactiveSlot {
    async fn is_complete(&self, context: &OperationContext<'_>) -> Result<bool, ActionError> {
        let image = planned_image(context)?;
        let node = node_object(context.store, &self.node).await?;
        fallen_back(&node, image)?;
        let Some(boot) = &node.status.boot else {
            return Err(ActionError::retryable(format!("{} does not report its boot slots", self.node)));
        };
        if let Some(reason) = boot.refusal_of(image) {
            return Err(ActionError::invalid(format!(
                "{} refused {}: {reason}",
                self.node,
                image.identity()
            )));
        }
        Ok(boot.runs(image) || boot.holds_in_inactive_slot(image))
    }

    async fn run(&self, context: &OperationContext<'_>) -> Result<(), ActionError> {
        request_image(context.store, &self.node, planned_image(context)?, false).await
    }
}

pub struct RebootIntoSlot {
    pub node: NodeName,
}

#[async_trait::async_trait]
impl Step for RebootIntoSlot {
    async fn is_complete(&self, context: &OperationContext<'_>) -> Result<bool, ActionError> {
        let image = planned_image(context)?;
        let node = node_object(context.store, &self.node).await?;
        fallen_back(&node, image)?;
        Ok(node.status.boot.as_ref().is_some_and(|boot| boot.runs(image)))
    }

    async fn run(&self, context: &OperationContext<'_>) -> Result<(), ActionError> {
        request_image(context.store, &self.node, planned_image(context)?, true).await
    }
}

pub struct AwaitHealthy {
    pub node: NodeName,
}

#[async_trait::async_trait]
impl Step for AwaitHealthy {
    async fn is_complete(&self, context: &OperationContext<'_>) -> Result<bool, ActionError> {
        let image = planned_image(context)?;
        let node = get::<Node>(context.store, &node_reference(&self.node), Consistency::Linearizable).await?;
        if let Some(node) = &node {
            fallen_back(node, image)?;
            if node.status.is_ready() && node.status.runs_confirmed(image) {
                return Ok(true);
            }
        }
        let deadline = context
            .operation
            .specification
            .upgrade_plan()
            .map_or(DEFAULT_HEALTH_DEADLINE_SECONDS, |plan| plan.health_deadline_seconds);
        if step_age(context) > Duration::from_secs(deadline) {
            return Err(ActionError::invalid(format!(
                "{} did not run {} healthily within {deadline} seconds",
                self.node,
                image.identity()
            )));
        }
        Ok(false)
    }

    async fn run(&self, _context: &OperationContext<'_>) -> Result<(), ActionError> {
        Ok(())
    }
}

pub struct Uncordon {
    pub node: NodeName,
}

#[async_trait::async_trait]
impl Step for Uncordon {
    async fn is_complete(&self, context: &OperationContext<'_>) -> Result<bool, ActionError> {
        Ok(!node_object(context.store, &self.node).await?.specification.is_cordoned)
    }

    async fn run(&self, context: &OperationContext<'_>) -> Result<(), ActionError> {
        change_node_specification(context.store, &self.node, |current| NodeSpecification {
            is_cordoned: false,
            ..current.clone()
        })
        .await
    }
}

pub struct Soak;

#[async_trait::async_trait]
impl Step for Soak {
    async fn is_complete(&self, context: &OperationContext<'_>) -> Result<bool, ActionError> {
        let soak = context.operation.specification.upgrade_plan().map_or(0, |plan| plan.soak_seconds);
        Ok(step_age(context) >= Duration::from_secs(soak))
    }

    async fn run(&self, _context: &OperationContext<'_>) -> Result<(), ActionError> {
        Ok(())
    }
}

#[must_use]
pub fn is_upgrade_in_progress(operations: &[Object<Operation>]) -> bool {
    operations.iter().any(|operation| {
        operation.specification.kind == OperationKind::Upgrade && !operation.status.is_finished()
    })
}
