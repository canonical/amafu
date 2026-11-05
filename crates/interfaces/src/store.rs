use amafu_domain::{ObjectReference, ProjectName};
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consistency {
    Linearizable,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredRecord {
    pub revision: u64,
    pub object: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    Changed(ObjectReference),
    Resynchronize,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Fence {
    pub name: String,
    pub token: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
pub enum StoreError {
    #[error("the object changed since it was read")]
    Conflict,
    #[error("the write carried fencing token {token} of {name}, older than one already accepted")]
    Fenced { name: String, token: u64 },
    #[error("the store refused the request: {0}")]
    Refused(String),
    #[error("the store is unavailable: {0}")]
    Unavailable(String),
    #[error("stored data is corrupt: {0}")]
    Corrupt(String),
}

pub type ObjectFilter<'filter> = dyn for<'object> Fn(&'object Value) -> bool + Send + Sync + 'filter;

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

    async fn list_matching(
        &self,
        kind: &str,
        project: Option<&ProjectName>,
        consistency: Consistency,
        matches: &ObjectFilter<'_>,
    ) -> Result<Vec<StoredRecord>, StoreError> {
        let mut records = self.list(kind, project, consistency).await?;
        records.retain(|record| matches(&record.object));
        Ok(records)
    }

    async fn compare_and_swap(
        &self,
        reference: &ObjectReference,
        expected_revision: Option<u64>,
        replacement: Option<Value>,
    ) -> Result<u64, StoreError>;

    async fn compare_and_swap_fenced(
        &self,
        reference: &ObjectReference,
        expected_revision: Option<u64>,
        replacement: Option<Value>,
        fence: &Fence,
    ) -> Result<u64, StoreError>;

    fn watch(&self, kind: &str) -> BoxStream<'static, WatchEvent>;
}
