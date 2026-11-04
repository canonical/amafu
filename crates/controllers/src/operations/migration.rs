use std::collections::BTreeMap;
use std::sync::Arc;

use amafu_domain::kinds::floating_ip::FloatingIp;
use amafu_domain::kinds::network_endpoint::NetworkEndpoint;
use amafu_domain::kinds::node::{Node, NodeSpecification};
use amafu_domain::kinds::operation::{
    Operation, OperationOutcome, OperationSpecification, OperationTarget, VmReference,
};
use amafu_domain::kinds::vm::{MigrationPhase, Vm, VmMigration, VmPhase, VmStatus};
use amafu_domain::{Change, ChangeError, NodeName, Object, ObjectId, ObjectName, ObjectReference};
use amafu_interfaces::objects::{apply, get, list, update_status_fenced};
use amafu_interfaces::{ActionError, Consistency, Identifiers, ObjectStoreError, Store};
use amafu_reconciler::{OperationContext, Step};

use crate::operations::node_specification::{change_node_specification, node_reference};
use crate::scheduling::{SchedulingSettings, choose_destination, observe_cluster};

pub const DOWNTIME_TARGET_MILLISECONDS: u64 = 100;

fn vm_reference(vm: &VmReference) -> ObjectReference {
    ObjectReference::of::<Vm>(Some(vm.project.clone()), vm.name.clone())
}

async fn vm_object(store: &dyn Store, vm: &VmReference) -> Result<Object<Vm>, ActionError> {
    get::<Vm>(store, &vm_reference(vm), Consistency::Linearizable)
        .await?
        .ok_or_else(|| ActionError::invalid(format!("VM {vm} does not exist")))
}

fn this_migration<'vm>(vm: &'vm Object<Vm>, context: &OperationContext<'_>) -> Option<&'vm VmMigration> {
    vm.status.migration.as_ref().filter(|migration| migration.attempt == context.operation.metadata.id)
}

fn reached(
    vm: &Object<Vm>,
    context: &OperationContext<'_>,
    phase: MigrationPhase,
) -> Result<bool, ActionError> {
    match this_migration(vm, context) {
        Some(migration) if migration.phase == MigrationPhase::Failed => Err(ActionError::invalid(format!(
            "the migration failed: {}",
            migration.message.as_deref().unwrap_or("no reason was given")
        ))),
        Some(migration) => Ok(migration.phase >= phase),
        None => Ok(false),
    }
}

async fn advance_migration(
    context: &OperationContext<'_>,
    vm: &VmReference,
    from: MigrationPhase,
    to: MigrationPhase,
) -> Result<(), ActionError> {
    let attempt = context.operation.metadata.id;
    update_status_fenced::<Vm>(context.store, &vm_reference(vm), context.fence, |current| {
        let migration = current.status.migration.as_ref().filter(|migration| migration.attempt == attempt)?;
        (migration.phase == from)
            .then(|| VmStatus { migration: Some(migration.at(to)), ..current.status.clone() })
    })
    .await?;
    Ok(())
}

async fn node_address(store: &dyn Store, node: &NodeName) -> Result<std::net::Ipv4Addr, ActionError> {
    let reference = ObjectReference::of::<Node>(None, ObjectName::from(node));
    get::<Node>(store, &reference, Consistency::Linearizable)
        .await?
        .and_then(|node| node.status.underlay_address)
        .ok_or_else(|| ActionError::retryable(format!("{node} has not reported its underlay address")))
}

pub struct PrepareDestination {
    pub vm: VmReference,
    pub destination: Option<NodeName>,
    pub scheduling: SchedulingSettings,
}

impl PrepareDestination {
    async fn destination(&self, store: &dyn Store, vm: &Object<Vm>) -> Result<NodeName, ActionError> {
        let cluster = observe_cluster(store).await?;
        if let Some(chosen) = &self.destination {
            let is_known = cluster.nodes.iter().any(|node| node.metadata.name.as_str() == chosen.as_str());
            return if is_known {
                Ok(chosen.clone())
            } else {
                Err(ActionError::invalid(format!("there is no node named {chosen}")))
            };
        }
        choose_destination(&cluster, vm, self.scheduling)
            .map_err(|reason| ActionError::retryable(format!("no node can take {}: {reason}", self.vm)))
    }
}

#[async_trait::async_trait]
impl Step for PrepareDestination {
    async fn is_complete(&self, context: &OperationContext<'_>) -> Result<bool, ActionError> {
        reached(&vm_object(context.store, &self.vm).await?, context, MigrationPhase::DestinationReady)
    }

    async fn run(&self, context: &OperationContext<'_>) -> Result<(), ActionError> {
        let vm = vm_object(context.store, &self.vm).await?;
        if this_migration(&vm, context).is_some() {
            return Ok(());
        }
        let source = vm
            .status
            .node
            .clone()
            .filter(|_| vm.status.phase == VmPhase::Running)
            .ok_or_else(|| ActionError::invalid(format!("VM {} is not running", self.vm)))?;
        if vm.status.is_migrating() {
            return Err(ActionError::invalid(format!("VM {} is already moving", self.vm)));
        }
        let destination = self.destination(context.store, &vm).await?;
        if destination == source {
            return Err(ActionError::invalid(format!("VM {} already runs on {destination}", self.vm)));
        }
        let migration = VmMigration {
            attempt: context.operation.metadata.id,
            source,
            destination_address: node_address(context.store, &destination).await?,
            destination,
            downtime_milliseconds: DOWNTIME_TARGET_MILLISECONDS,
            phase: MigrationPhase::Preparing,
            report: None,
            message: None,
        };
        update_status_fenced::<Vm>(context.store, &vm.reference(), context.fence, |current| {
            (!current.status.is_migrating())
                .then(|| VmStatus { migration: Some(migration.clone()), ..current.status.clone() })
        })
        .await?;
        Ok(())
    }

    async fn undo(&self, context: &OperationContext<'_>) -> Result<(), ActionError> {
        let attempt = context.operation.metadata.id;
        update_status_fenced::<Vm>(context.store, &vm_reference(&self.vm), context.fence, |current| {
            let migration =
                current.status.migration.as_ref().filter(|migration| migration.attempt == attempt)?;
            let has_moved = matches!(migration.phase, MigrationPhase::SwitchedOver | MigrationPhase::Cleaned);
            (!has_moved).then(|| VmStatus { migration: None, ..current.status.clone() })
        })
        .await
        .map(|_| ())
        .or_else(|error| match error {
            ObjectStoreError::Change(ChangeError::NotFound) => Ok(()),
            other => Err(other.into()),
        })
    }
}

pub struct CopyMemory {
    pub vm: VmReference,
}

#[async_trait::async_trait]
impl Step for CopyMemory {
    async fn is_complete(&self, context: &OperationContext<'_>) -> Result<bool, ActionError> {
        reached(&vm_object(context.store, &self.vm).await?, context, MigrationPhase::Copied)
    }

    async fn run(&self, context: &OperationContext<'_>) -> Result<(), ActionError> {
        advance_migration(context, &self.vm, MigrationPhase::DestinationReady, MigrationPhase::Copying).await
    }
}

pub struct SwitchOver {
    pub vm: VmReference,
}

#[async_trait::async_trait]
impl Step for SwitchOver {
    async fn is_complete(&self, context: &OperationContext<'_>) -> Result<bool, ActionError> {
        let vm = vm_object(context.store, &self.vm).await?;
        let switched = reached(&vm, context, MigrationPhase::SwitchedOver)?;
        let destination = this_migration(&vm, context).map(|migration| &migration.destination);
        Ok(switched && vm.status.node.as_ref() == destination)
    }

    async fn run(&self, context: &OperationContext<'_>) -> Result<(), ActionError> {
        let attempt = context.operation.metadata.id;
        update_status_fenced::<Vm>(context.store, &vm_reference(&self.vm), context.fence, |current| {
            let migration =
                current.status.migration.as_ref().filter(|migration| migration.attempt == attempt)?;
            (migration.phase == MigrationPhase::Copied).then(|| VmStatus {
                node: Some(migration.destination.clone()),
                migration: Some(migration.at(MigrationPhase::SwitchedOver)),
                ..current.status.clone()
            })
        })
        .await?;
        tracing::info!(vm = %self.vm, "a VM switched over to its destination");
        Ok(())
    }
}

pub struct AnnounceAddresses {
    pub vm: VmReference,
}

#[async_trait::async_trait]
impl Step for AnnounceAddresses {
    async fn is_complete(&self, context: &OperationContext<'_>) -> Result<bool, ActionError> {
        let store = context.store;
        let vm = vm_object(store, &self.vm).await?;
        let Some(destination) = this_migration(&vm, context).map(|migration| migration.destination.clone())
        else {
            return Ok(false);
        };
        let endpoints =
            list::<NetworkEndpoint>(store, Some(&self.vm.project), Consistency::Linearizable).await?;
        let endpoints_moved = (0..vm.status.interfaces.len()).all(|index| {
            let name = NetworkEndpoint::name_for(vm.metadata.id, u8::try_from(index).unwrap_or(u8::MAX));
            endpoints.iter().any(|endpoint| {
                endpoint.metadata.name == name
                    && endpoint.specification.node == destination
                    && !endpoint.metadata.is_deleting
            })
        });
        let floating_ips =
            list::<FloatingIp>(store, Some(&self.vm.project), Consistency::Linearizable).await?;
        let floating_moved = floating_ips
            .iter()
            .filter_map(|floating_ip| floating_ip.status.binding.as_ref())
            .filter(|binding| binding.vm_id == vm.metadata.id)
            .all(|binding| binding.node == destination);
        Ok(endpoints_moved && floating_moved)
    }

    async fn run(&self, _context: &OperationContext<'_>) -> Result<(), ActionError> {
        Ok(())
    }
}

pub struct CleanUpSource {
    pub vm: VmReference,
}

#[async_trait::async_trait]
impl Step for CleanUpSource {
    async fn is_complete(&self, context: &OperationContext<'_>) -> Result<bool, ActionError> {
        reached(&vm_object(context.store, &self.vm).await?, context, MigrationPhase::Cleaned)
    }

    async fn run(&self, _context: &OperationContext<'_>) -> Result<(), ActionError> {
        Ok(())
    }
}

pub struct Cordon {
    pub node: NodeName,
}

impl Cordon {
    async fn set_cordoned(&self, store: &dyn Store, is_cordoned: bool) -> Result<(), ActionError> {
        change_node_specification(store, &self.node, |current| NodeSpecification {
            is_cordoned,
            ..current.clone()
        })
        .await
    }
}

#[async_trait::async_trait]
impl Step for Cordon {
    async fn is_complete(&self, context: &OperationContext<'_>) -> Result<bool, ActionError> {
        let node = get::<Node>(context.store, &node_reference(&self.node), Consistency::Linearizable).await?;
        Ok(node.is_some_and(|node| node.specification.is_cordoned))
    }

    async fn run(&self, context: &OperationContext<'_>) -> Result<(), ActionError> {
        self.set_cordoned(context.store, true).await
    }

    async fn undo(&self, context: &OperationContext<'_>) -> Result<(), ActionError> {
        self.set_cordoned(context.store, false).await
    }
}

pub struct MigrateVm {
    pub vm: VmReference,
    pub identifiers: Arc<dyn Identifiers>,
}

fn evacuated_node(context: &OperationContext<'_>) -> Result<NodeName, ActionError> {
    match &context.operation.specification.target {
        OperationTarget::Node { node } => Ok(node.clone()),
        OperationTarget::Vm { .. } | OperationTarget::Cluster => {
            Err(ActionError::fatal("only an evacuation moves VMs off a node"))
        }
    }
}

pub fn child_name(
    context: &OperationContext<'_>,
    prefix: &str,
    subject: &str,
) -> Result<ObjectName, ActionError> {
    let parent = context.operation.metadata.id.to_string();
    let parent_prefix = parent.get(..8).unwrap_or(&parent);
    ObjectName::new(format!("{prefix}-{subject}-{parent_prefix}")).map_err(ActionError::fatal)
}

fn migration_child_name(context: &OperationContext<'_>, vm_id: ObjectId) -> Result<ObjectName, ActionError> {
    child_name(context, "migrate", &vm_id.to_string().replace('-', ""))
}

#[async_trait::async_trait]
impl Step for MigrateVm {
    async fn is_complete(&self, context: &OperationContext<'_>) -> Result<bool, ActionError> {
        let Some(vm) = get::<Vm>(context.store, &vm_reference(&self.vm), Consistency::Linearizable).await?
        else {
            return Ok(true);
        };
        let evacuated = evacuated_node(context)?;
        let has_left = vm.status.node.as_ref() != Some(&evacuated) && !vm.status.is_migrating();
        if has_left {
            return Ok(true);
        }
        let reference =
            ObjectReference::of::<Operation>(None, migration_child_name(context, vm.metadata.id)?);
        match get::<Operation>(context.store, &reference, Consistency::Linearizable).await? {
            Some(child) => match &child.status.outcome {
                Some(OperationOutcome::Failed { reason }) => {
                    Err(ActionError::retryable(format!("moving {} failed: {reason}", self.vm)))
                }
                _ => Ok(false),
            },
            None => Ok(false),
        }
    }

    async fn run(&self, context: &OperationContext<'_>) -> Result<(), ActionError> {
        let vm = vm_object(context.store, &self.vm).await?;
        let name = migration_child_name(context, vm.metadata.id)?;
        let reference = ObjectReference::of::<Operation>(None, name.clone());
        if get::<Operation>(context.store, &reference, Consistency::Linearizable).await?.is_some() {
            return Ok(());
        }
        apply::<Operation>(
            context.store,
            &reference,
            Change::Create {
                id: ObjectId::from_uuid(self.identifiers.generate()),
                name,
                project: None,
                labels: BTreeMap::new(),
                specification: OperationSpecification::migration(self.vm.clone(), None),
            },
        )
        .await?;
        Ok(())
    }
}
