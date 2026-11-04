use std::collections::BTreeMap;

use amafu_domain::kinds::node_lease::{
    LeaseRefusal, LeaseTerms, NodeLease, NodeLeaseSpecification, node_fence_name,
};
use amafu_domain::{Change, NodeName, Object, ObjectId, ObjectName, ObjectReference, apply_change};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use amafu_domain::kinds::node::{Node, NodePhase, NodeStatus};

use crate::objects::{ObjectStoreError, update_status_fenced};
use crate::store::{Consistency, Fence, Store, StoreError};

const CONTENDED_ATTEMPTS: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeLeaseGrant {
    pub epoch: u64,
    pub terms: LeaseTerms,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum NodeLeaseError {
    #[error(transparent)]
    Refused(#[from] LeaseRefusal),
    #[error(transparent)]
    Store(#[from] ObjectStoreError),
}

impl NodeLeaseError {
    #[must_use]
    pub fn is_refusal(&self) -> bool {
        matches!(
            self,
            Self::Refused(_)
                | Self::Store(ObjectStoreError::Store(StoreError::Fenced { .. } | StoreError::Refused(_)))
        )
    }
}

impl From<StoreError> for NodeLeaseError {
    fn from(error: StoreError) -> Self {
        Self::Store(ObjectStoreError::Store(error))
    }
}

#[async_trait::async_trait]
pub trait NodeLeaseAuthority: Send + Sync {
    async fn acquire(&self, terms: LeaseTerms) -> Result<NodeLeaseGrant, NodeLeaseError>;

    async fn renew(&self, epoch: u64) -> Result<NodeLeaseGrant, NodeLeaseError>;

    async fn surrender(&self, epoch: u64) -> Result<(), NodeLeaseError>;
}

#[must_use]
pub fn node_fence(node: &NodeName, epoch: u64) -> Fence {
    Fence { name: node_fence_name(node), token: epoch }
}

#[must_use]
pub fn lease_reference(node: &NodeName) -> ObjectReference {
    ObjectReference::of::<NodeLease>(None, ObjectName::from(node))
}

pub async fn read_node_lease(
    store: &dyn Store,
    node: &NodeName,
) -> Result<Option<(u64, Object<NodeLease>)>, NodeLeaseError> {
    let Some(record) = store.read(&lease_reference(node), Consistency::Linearizable).await? else {
        return Ok(None);
    };
    let lease = serde_json::from_value(record.object)
        .map_err(|error| NodeLeaseError::Store(ObjectStoreError::Corrupt(error.to_string())))?;
    Ok(Some((record.revision, lease)))
}

pub async fn change_node_lease(
    store: &dyn Store,
    node: &NodeName,
    id_if_created: Option<ObjectId>,
    compute: impl Fn(Option<&NodeLeaseSpecification>) -> Result<NodeLeaseSpecification, LeaseRefusal>
    + Send
    + Sync,
) -> Result<NodeLeaseSpecification, NodeLeaseError> {
    let reference = lease_reference(node);
    for _ in 0..CONTENDED_ATTEMPTS {
        let current = read_node_lease(store, node).await?;
        let specification = compute(current.as_ref().map(|(_, lease)| &lease.specification))?;
        let (expected_revision, existing, change) = match current {
            None => {
                let id = id_if_created.ok_or(LeaseRefusal::Lost)?;
                let create = Change::Create {
                    id,
                    name: reference.name.clone(),
                    project: None,
                    labels: BTreeMap::new(),
                    specification: specification.clone(),
                };
                (None, None, create)
            }
            Some((revision, lease)) => {
                let update = Change::UpdateSpecification {
                    expected_generation: Some(lease.metadata.generation),
                    specification: specification.clone(),
                };
                (Some(revision), Some(lease), update)
            }
        };
        let updated = apply_change(existing, change).map_err(ObjectStoreError::from)?;
        let encoded =
            serde_json::to_value(&updated).map_err(|error| ObjectStoreError::Corrupt(error.to_string()))?;
        let fence = node_fence(node, specification.epoch);
        match store.compare_and_swap_fenced(&reference, expected_revision, Some(encoded), &fence).await {
            Ok(_) => return Ok(specification),
            Err(StoreError::Conflict) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(NodeLeaseError::Store(ObjectStoreError::Contended))
}

pub async fn acquire_node_lease(
    store: &dyn Store,
    node: &NodeName,
    new_id: ObjectId,
    terms: LeaseTerms,
    now: OffsetDateTime,
) -> Result<NodeLeaseGrant, NodeLeaseError> {
    let lease = change_node_lease(store, node, Some(new_id), |existing| {
        NodeLeaseSpecification::acquired(existing, terms, now)
    })
    .await?;
    Ok(NodeLeaseGrant { epoch: lease.epoch, terms: lease.terms })
}

pub async fn renew_node_lease(
    store: &dyn Store,
    node: &NodeName,
    epoch: u64,
    now: OffsetDateTime,
) -> Result<NodeLeaseGrant, NodeLeaseError> {
    let lease = change_node_lease(store, node, None, |existing| {
        existing.ok_or(LeaseRefusal::Lost)?.renewed(epoch, now)
    })
    .await?;
    Ok(NodeLeaseGrant { epoch: lease.epoch, terms: lease.terms })
}

pub async fn surrender_node_lease(
    store: &dyn Store,
    node: &NodeName,
    epoch: u64,
) -> Result<NodeLeaseGrant, NodeLeaseError> {
    let surrendered = change_node_lease(store, node, None, |existing| {
        let lease = existing.ok_or(LeaseRefusal::Lost)?;
        if lease.is_lost && lease.is_released && lease.epoch == epoch + 1 {
            return Ok(lease.clone());
        }
        lease.surrendered(epoch)
    })
    .await?;
    let reference = ObjectReference::of::<Node>(None, ObjectName::from(node));
    update_status_fenced::<Node>(store, &reference, &node_fence(node, surrendered.epoch), |current| {
        (current.status.phase != NodePhase::NotReady)
            .then(|| NodeStatus { phase: NodePhase::NotReady, ..current.status.clone() })
    })
    .await
    .map(|_| ())
    .or_else(|error| match error {
        ObjectStoreError::Change(amafu_domain::ChangeError::NotFound) => Ok(()),
        other => Err(NodeLeaseError::from(other)),
    })?;
    Ok(NodeLeaseGrant { epoch: surrendered.epoch, terms: surrendered.terms })
}

pub async fn admit_node_write(store: &dyn Store, node: &NodeName, epoch: u64) -> Result<(), NodeLeaseError> {
    let Some((_, lease)) = read_node_lease(store, node).await? else {
        return Err(NodeLeaseError::Refused(LeaseRefusal::Lost));
    };
    Ok(lease.specification.admits(epoch)?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeldNodeLease {
    pub epoch: u64,
    pub valid_until: OffsetDateTime,
}

#[derive(Debug, Clone, Default)]
pub struct NodeLeaseState {
    held: std::sync::Arc<std::sync::Mutex<Option<HeldNodeLease>>>,
    is_surrendered: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl NodeLeaseState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn hold(&self, lease: HeldNodeLease) {
        *self.lock() = Some(lease);
    }

    pub fn drop_lease(&self) {
        *self.lock() = None;
    }

    #[must_use]
    pub fn surrender(&self) -> Option<HeldNodeLease> {
        self.is_surrendered.store(true, std::sync::atomic::Ordering::SeqCst);
        self.lock().take()
    }

    #[must_use]
    pub fn is_surrendered(&self) -> bool {
        self.is_surrendered.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[must_use]
    pub fn epoch_valid_at(&self, now: OffsetDateTime) -> Option<u64> {
        self.lock().filter(|lease| now < lease.valid_until).map(|lease| lease.epoch)
    }

    #[must_use]
    pub fn held(&self) -> Option<HeldNodeLease> {
        *self.lock()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<HeldNodeLease>> {
        self.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lease_counts_only_until_it_runs_out() {
        let state = NodeLeaseState::new();
        let start = OffsetDateTime::UNIX_EPOCH;
        assert_eq!(state.epoch_valid_at(start), None);
        state.hold(HeldNodeLease { epoch: 3, valid_until: start + time::Duration::seconds(8) });
        assert_eq!(state.epoch_valid_at(start + time::Duration::seconds(7)), Some(3));
        assert_eq!(state.epoch_valid_at(start + time::Duration::seconds(8)), None);
        state.drop_lease();
        assert_eq!(state.epoch_valid_at(start), None);
    }
}
