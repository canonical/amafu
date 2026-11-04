use std::sync::Arc;

use amafu_domain::kinds::node_lease::LeaseTerms;
use amafu_domain::{NodeName, ObjectId, ObjectReference, ProjectName};
use amafu_interfaces::node_lease::{acquire_node_lease, node_fence, renew_node_lease, surrender_node_lease};
use amafu_interfaces::{
    Clock, Consistency, Fence, Identifiers, NodeLeaseAuthority, NodeLeaseError, NodeLeaseGrant,
    NodeLeaseState, Store, StoreError, StoredRecord, WatchEvent,
};
use futures::stream::BoxStream;
use serde_json::Value;

pub struct LeaseFencedStore {
    inner: Arc<dyn Store>,
    node: NodeName,
    lease: NodeLeaseState,
    clock: Arc<dyn Clock>,
}

impl LeaseFencedStore {
    #[must_use]
    pub fn new(inner: Arc<dyn Store>, node: NodeName, lease: NodeLeaseState, clock: Arc<dyn Clock>) -> Self {
        Self { inner, node, lease, clock }
    }

    fn current_fence(&self) -> Result<Fence, StoreError> {
        self.lease
            .epoch_valid_at(self.clock.now())
            .map(|epoch| node_fence(&self.node, epoch))
            .ok_or_else(|| StoreError::Refused(String::from("the node holds no valid lease")))
    }
}

#[async_trait::async_trait]
impl Store for LeaseFencedStore {
    async fn read(
        &self,
        reference: &ObjectReference,
        consistency: Consistency,
    ) -> Result<Option<StoredRecord>, StoreError> {
        self.inner.read(reference, consistency).await
    }

    async fn list(
        &self,
        kind: &str,
        project: Option<&ProjectName>,
        consistency: Consistency,
    ) -> Result<Vec<StoredRecord>, StoreError> {
        self.inner.list(kind, project, consistency).await
    }

    async fn compare_and_swap(
        &self,
        reference: &ObjectReference,
        expected_revision: Option<u64>,
        replacement: Option<Value>,
    ) -> Result<u64, StoreError> {
        let fence = self.current_fence()?;
        self.inner.compare_and_swap_fenced(reference, expected_revision, replacement, &fence).await
    }

    async fn compare_and_swap_fenced(
        &self,
        _reference: &ObjectReference,
        _expected_revision: Option<u64>,
        _replacement: Option<Value>,
        _fence: &Fence,
    ) -> Result<u64, StoreError> {
        Err(StoreError::Refused(String::from("a node's own roles write only under the node's lease")))
    }

    fn watch(&self, kind: &str) -> BoxStream<'static, WatchEvent> {
        self.inner.watch(kind)
    }
}

pub struct LocalNodeLeases {
    store: Arc<dyn Store>,
    node: NodeName,
    clock: Arc<dyn Clock>,
    identifiers: Arc<dyn Identifiers>,
}

impl LocalNodeLeases {
    #[must_use]
    pub fn new(
        store: Arc<dyn Store>,
        node: NodeName,
        clock: Arc<dyn Clock>,
        identifiers: Arc<dyn Identifiers>,
    ) -> Self {
        Self { store, node, clock, identifiers }
    }
}

#[async_trait::async_trait]
impl NodeLeaseAuthority for LocalNodeLeases {
    async fn acquire(&self, terms: LeaseTerms) -> Result<NodeLeaseGrant, NodeLeaseError> {
        let id = ObjectId::from_uuid(self.identifiers.generate());
        acquire_node_lease(self.store.as_ref(), &self.node, id, terms, self.clock.now()).await
    }

    async fn renew(&self, epoch: u64) -> Result<NodeLeaseGrant, NodeLeaseError> {
        renew_node_lease(self.store.as_ref(), &self.node, epoch, self.clock.now()).await
    }

    async fn surrender(&self, epoch: u64) -> Result<(), NodeLeaseError> {
        surrender_node_lease(self.store.as_ref(), &self.node, epoch).await.map(|_| ())
    }
}
