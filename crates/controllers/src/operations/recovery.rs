use std::sync::Arc;

use amafu_domain::kinds::network_endpoint::NetworkEndpoint;
use amafu_domain::kinds::node::{Node, NodePhase, NodeStatus};
use amafu_domain::kinds::node_lease::{LeaseRefusal, NodeLease};
use amafu_domain::kinds::storage_backend::StorageBackend;
use amafu_domain::kinds::vm::{Vm, VmPhase, VmStatus};
use amafu_domain::{Change, NodeName, Object, ObjectName, ObjectReference};
use amafu_interfaces::node_lease::{change_node_lease, lease_reference, node_fence};
use amafu_interfaces::objects::{apply_fenced, get, list, remove, update_status_fenced};
use amafu_interfaces::{
    ActionError, Consistency, FencedNode, NodeLeaseError, Storage, StorageProvider, Store,
};
use amafu_reconciler::{OperationContext, Step};

fn lease_failure(error: NodeLeaseError) -> ActionError {
    match error {
        NodeLeaseError::Refused(LeaseRefusal::NotExpired) => {
            ActionError::invalid("the node renewed its lease, so it is not lost")
        }
        NodeLeaseError::Refused(refusal) => ActionError::invalid(refusal),
        NodeLeaseError::Store(error) => error.into(),
    }
}

async fn lease_of(store: &dyn Store, node: &NodeName) -> Result<Object<NodeLease>, ActionError> {
    get::<NodeLease>(store, &lease_reference(node), Consistency::Linearizable)
        .await?
        .ok_or_else(|| ActionError::invalid(format!("{node} has never held a lease")))
}

async fn node_object(store: &dyn Store, node: &NodeName) -> Result<Option<Object<Node>>, ActionError> {
    let reference = ObjectReference::of::<Node>(None, ObjectName::from(node));
    Ok(get::<Node>(store, &reference, Consistency::Linearizable).await?)
}

pub struct MarkNodeLost {
    pub node: NodeName,
}

#[async_trait::async_trait]
impl Step for MarkNodeLost {
    async fn is_complete(&self, context: &OperationContext<'_>) -> Result<bool, ActionError> {
        let lease = lease_of(context.store, &self.node).await?;
        let node = node_object(context.store, &self.node).await?;
        Ok(lease.specification.is_lost && node.is_none_or(|node| node.status.phase == NodePhase::Lost))
    }

    async fn run(&self, context: &OperationContext<'_>) -> Result<(), ActionError> {
        let now = context.clock.now();
        let lease = change_node_lease(context.store, &self.node, None, |existing| {
            let lease = existing.ok_or(LeaseRefusal::Lost)?;
            if lease.is_lost { Ok(lease.clone()) } else { lease.marked_lost(now) }
        })
        .await
        .map_err(lease_failure)?;
        let fence = node_fence(&self.node, lease.epoch);
        let reference = ObjectReference::of::<Node>(None, ObjectName::from(&self.node));
        if node_object(context.store, &self.node).await?.is_some() {
            update_status_fenced::<Node>(context.store, &reference, &fence, |node| {
                (node.status.phase != NodePhase::Lost)
                    .then(|| NodeStatus { phase: NodePhase::Lost, ..node.status.clone() })
            })
            .await?;
        }
        tracing::warn!(node = %self.node, epoch = lease.epoch, "a node was marked lost");
        Ok(())
    }
}

pub struct RevokeNodeOnStorage {
    pub node: NodeName,
    pub storage: Arc<dyn Storage>,
}

impl RevokeNodeOnStorage {
    async fn providers(&self, store: &dyn Store) -> Result<Vec<Arc<dyn StorageProvider>>, ActionError> {
        list::<StorageBackend>(store, None, Consistency::Linearizable)
            .await?
            .iter()
            .map(|backend| self.storage.provider(&backend.specification.provider))
            .collect()
    }
}

pub async fn fenced_node(store: &dyn Store, node: &NodeName) -> Result<FencedNode, ActionError> {
    let address = node_object(store, node).await?.and_then(|node| node.status.underlay_address);
    Ok(FencedNode { name: node.clone(), address })
}

#[async_trait::async_trait]
impl Step for RevokeNodeOnStorage {
    async fn is_complete(&self, context: &OperationContext<'_>) -> Result<bool, ActionError> {
        let fenced = fenced_node(context.store, &self.node).await?;
        for provider in self.providers(context.store).await? {
            if !provider.has_revoked_node(&fenced).await? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn run(&self, context: &OperationContext<'_>) -> Result<(), ActionError> {
        let fenced = fenced_node(context.store, &self.node).await?;
        for provider in self.providers(context.store).await? {
            provider.revoke_node(&fenced).await?;
        }
        tracing::warn!(node = %self.node, "every storage backend refuses the lost node");
        Ok(())
    }
}

pub struct WaitOutWatchdog {
    pub node: NodeName,
}

#[async_trait::async_trait]
impl Step for WaitOutWatchdog {
    async fn is_complete(&self, context: &OperationContext<'_>) -> Result<bool, ActionError> {
        let lease = lease_of(context.store, &self.node).await?;
        Ok(context.clock.now() >= lease.specification.watchdog_deadline())
    }

    async fn run(&self, _context: &OperationContext<'_>) -> Result<(), ActionError> {
        Ok(())
    }
}

pub struct ReleaseAssignments {
    pub node: NodeName,
    pub storage: Arc<dyn Storage>,
}

#[must_use]
pub fn released(status: &VmStatus, node: &NodeName) -> VmStatus {
    VmStatus {
        phase: VmPhase::Pending,
        node: None,
        message: Some(format!("{node} was lost; the VM waits for another node")),
        ..status.clone()
    }
}

impl ReleaseAssignments {
    async fn assigned_vms(&self, store: &dyn Store) -> Result<Vec<Object<Vm>>, ActionError> {
        let vms = list::<Vm>(store, None, Consistency::Linearizable).await?;
        Ok(vms.into_iter().filter(|vm| vm.status.node.as_ref() == Some(&self.node)).collect())
    }

    async fn published_endpoints(
        &self,
        store: &dyn Store,
    ) -> Result<Vec<Object<NetworkEndpoint>>, ActionError> {
        let endpoints = list::<NetworkEndpoint>(store, None, Consistency::Linearizable).await?;
        Ok(endpoints.into_iter().filter(|endpoint| endpoint.specification.node == self.node).collect())
    }

    async fn readmit_on_storage(&self, store: &dyn Store) -> Result<(), ActionError> {
        let fenced = fenced_node(store, &self.node).await?;
        for backend in list::<StorageBackend>(store, None, Consistency::Linearizable).await? {
            self.storage.provider(&backend.specification.provider)?.readmit_node(&fenced).await?;
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Step for ReleaseAssignments {
    async fn is_complete(&self, context: &OperationContext<'_>) -> Result<bool, ActionError> {
        let lease = lease_of(context.store, &self.node).await?;
        Ok(lease.specification.is_released
            && self.assigned_vms(context.store).await?.is_empty()
            && self.published_endpoints(context.store).await?.is_empty())
    }

    async fn run(&self, context: &OperationContext<'_>) -> Result<(), ActionError> {
        let store = context.store;
        let lease = lease_of(store, &self.node).await?;
        let fence = node_fence(&self.node, lease.specification.epoch);
        for vm in self.assigned_vms(store).await? {
            let node = self.node.clone();
            update_status_fenced::<Vm>(store, &vm.reference(), &fence, |current| {
                (current.status.node.as_ref() == Some(&node)).then(|| released(&current.status, &node))
            })
            .await?;
            tracing::warn!(vm = %vm.reference(), node = %self.node, "a VM of a lost node waits for another node");
        }
        for endpoint in self.published_endpoints(store).await? {
            let reference = endpoint.reference();
            if !endpoint.metadata.is_deleting {
                apply_fenced::<NetworkEndpoint>(store, &reference, &fence, Change::Delete).await?;
            }
            remove::<NetworkEndpoint>(store, &reference).await?;
        }
        self.readmit_on_storage(store).await?;
        change_node_lease(store, &self.node, None, |existing| {
            let lease = existing.ok_or(LeaseRefusal::Lost)?;
            Ok(lease.released())
        })
        .await
        .map_err(lease_failure)?;
        Ok(())
    }
}
