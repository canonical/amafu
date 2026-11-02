use amafu_domain::{ObjectReference, ProjectName};
use futures::stream::BoxStream;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consistency {
    Linearizable,
    Stale,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredRecord {
    pub revision: u64,
    pub object: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    Changed(ObjectReference),
    Resynchronize,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    #[error("the object changed since it was read")]
    Conflict,
    #[error("the store is unavailable: {0}")]
    Unavailable(String),
    #[error("stored data is corrupt: {0}")]
    Corrupt(String),
}

#[async_trait::async_trait]
pub trait Store: Send + Sync {
    async fn read(
        &self,
        reference: &ObjectReference,
        consistency: Consistency,
    ) -> Result<Option<StoredRecord>, StoreError>;

    async fn list(
        &self,
        kind: &str,
        project: Option<&ProjectName>,
        consistency: Consistency,
    ) -> Result<Vec<StoredRecord>, StoreError>;

    async fn compare_and_swap(
        &self,
        reference: &ObjectReference,
        expected_revision: Option<u64>,
        replacement: Option<Value>,
    ) -> Result<u64, StoreError>;

    fn watch(&self, kind: &str) -> BoxStream<'static, WatchEvent>;
}
