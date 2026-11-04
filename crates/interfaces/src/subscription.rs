use amafu_domain::{ObjectReference, ProjectName};
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::store::{StoreError, StoredRecord};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SliceRecord {
    pub reference: ObjectReference,
    pub record: StoredRecord,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum SliceChange {
    Snapshot { records: Vec<SliceRecord> },
    Upserted { record: SliceRecord },
    Removed { reference: ObjectReference },
    Heartbeat,
}

pub type SliceChanges = BoxStream<'static, Result<SliceChange, StoreError>>;

#[async_trait::async_trait]
pub trait CoreConnection: Send + Sync {
    async fn read(&self, reference: &ObjectReference) -> Result<Option<StoredRecord>, StoreError>;

    async fn list(&self, kind: &str, project: Option<&ProjectName>) -> Result<Vec<StoredRecord>, StoreError>;

    async fn compare_and_swap(
        &self,
        reference: &ObjectReference,
        expected_revision: Option<u64>,
        replacement: Option<Value>,
        epoch: u64,
    ) -> Result<u64, StoreError>;

    async fn subscribe(&self) -> Result<SliceChanges, StoreError>;
}
