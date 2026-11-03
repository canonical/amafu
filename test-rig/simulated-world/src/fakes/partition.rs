use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use amafu_api::client::{ClientError, ResponseBody, Transport};
use amafu_domain::{ObjectReference, ProjectName};
use amafu_interfaces::{Consistency, Fence, Store, StoreError, StoredRecord, WatchEvent};
use axum::body::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use http::{Request, Response};
use serde_json::Value;

#[derive(Debug, Default)]
pub struct NodeLink {
    is_cut: AtomicBool,
}

impl NodeLink {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn cut(&self) {
        self.is_cut.store(true, Ordering::SeqCst);
    }

    pub fn restore(&self) {
        self.is_cut.store(false, Ordering::SeqCst);
    }

    #[must_use]
    pub fn is_cut(&self) -> bool {
        self.is_cut.load(Ordering::SeqCst)
    }

    fn check(&self) -> Result<(), StoreError> {
        if self.is_cut() {
            Err(StoreError::Unavailable(String::from("the node is partitioned")))
        } else {
            Ok(())
        }
    }
}

pub struct PartitionedStore {
    inner: Arc<dyn Store>,
    link: Arc<NodeLink>,
}

impl PartitionedStore {
    #[must_use]
    pub fn new(inner: Arc<dyn Store>, link: Arc<NodeLink>) -> Self {
        Self { inner, link }
    }
}

#[async_trait::async_trait]
impl Store for PartitionedStore {
    async fn read(
        &self,
        reference: &ObjectReference,
        consistency: Consistency,
    ) -> Result<Option<StoredRecord>, StoreError> {
        self.link.check()?;
        self.inner.read(reference, consistency).await
    }

    async fn list(
        &self,
        kind: &str,
        project: Option<&ProjectName>,
        consistency: Consistency,
    ) -> Result<Vec<StoredRecord>, StoreError> {
        self.link.check()?;
        self.inner.list(kind, project, consistency).await
    }

    async fn compare_and_swap(
        &self,
        reference: &ObjectReference,
        expected_revision: Option<u64>,
        replacement: Option<Value>,
    ) -> Result<u64, StoreError> {
        self.link.check()?;
        self.inner.compare_and_swap(reference, expected_revision, replacement).await
    }

    async fn compare_and_swap_fenced(
        &self,
        reference: &ObjectReference,
        expected_revision: Option<u64>,
        replacement: Option<Value>,
        fence: &Fence,
    ) -> Result<u64, StoreError> {
        self.link.check()?;
        self.inner.compare_and_swap_fenced(reference, expected_revision, replacement, fence).await
    }

    fn watch(&self, kind: &str) -> BoxStream<'static, WatchEvent> {
        self.inner.watch(kind)
    }
}

pub struct PartitionedTransport {
    inner: Arc<dyn Transport>,
    link: Arc<NodeLink>,
}

impl PartitionedTransport {
    #[must_use]
    pub fn new(inner: Arc<dyn Transport>, link: Arc<NodeLink>) -> Self {
        Self { inner, link }
    }
}

#[async_trait::async_trait]
impl Transport for PartitionedTransport {
    async fn send(&self, request: Request<Bytes>) -> Result<Response<ResponseBody>, ClientError> {
        if self.link.is_cut() {
            return Err(ClientError::Transport(String::from("the node is partitioned")));
        }
        let response = self.inner.send(request).await?;
        let link = Arc::clone(&self.link);
        Ok(response.map(|body| {
            body.map(move |chunk| {
                if link.is_cut() {
                    Err(ClientError::Transport(String::from("the node is partitioned")))
                } else {
                    chunk
                }
            })
            .boxed()
        }))
    }
}
