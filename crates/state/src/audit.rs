use amafu_interfaces::{AuditEntry, AuditLog, StoreError};
use quorium::Quorium;
use uuid::Uuid;

use crate::entities::{AUDIT_SEGMENT_CAPACITY, AuditCursor, AuditSegment};
use crate::errors::store_error;

const SEARCHED_SEGMENTS: u64 = 16;

#[derive(Clone)]
pub struct QuoriumAuditLog {
    node: Quorium,
}

impl QuoriumAuditLog {
    #[must_use]
    pub fn new(node: Quorium) -> Self {
        Self { node }
    }

    async fn segment_entries(&self, segment: u64) -> Result<Vec<AuditEntry>, StoreError> {
        let mut entries = self.node.entity::<AuditSegment>(segment).entries().await.map_err(store_error)?;
        entries.sort_by_key(|(sequence, _)| *sequence);
        entries
            .iter()
            .map(|(_, bytes)| {
                serde_json::from_slice(bytes).map_err(|error| StoreError::Corrupt(error.to_string()))
            })
            .collect()
    }

    async fn newest_segments(&self) -> Result<impl Iterator<Item = u64>, StoreError> {
        let allocated = self.node.global::<AuditCursor>().allocated().await.map_err(store_error)?;
        let newest = allocated.div_ceil(AUDIT_SEGMENT_CAPACITY);
        Ok((newest.saturating_sub(SEARCHED_SEGMENTS)..newest).rev())
    }
}

#[async_trait::async_trait]
impl AuditLog for QuoriumAuditLog {
    async fn record(&self, entry: AuditEntry) -> Result<(), StoreError> {
        let bytes = serde_json::to_vec(&entry).map_err(|error| StoreError::Corrupt(error.to_string()))?;
        let sequence = self.node.global::<AuditCursor>().allocate().await.map_err(store_error)?;
        self.node
            .entity::<AuditSegment>(sequence / AUDIT_SEGMENT_CAPACITY)
            .append(sequence, bytes)
            .await
            .map_err(store_error)
    }

    async fn find(&self, request_id: Uuid) -> Result<Option<AuditEntry>, StoreError> {
        for segment in self.newest_segments().await? {
            if let Some(entry) =
                self.segment_entries(segment).await?.into_iter().find(|entry| entry.request_id == request_id)
            {
                return Ok(Some(entry));
            }
        }
        Ok(None)
    }

    async fn recent(&self, limit: usize) -> Result<Vec<AuditEntry>, StoreError> {
        let mut recent = Vec::new();
        for segment in self.newest_segments().await? {
            for entry in self.segment_entries(segment).await?.into_iter().rev() {
                if recent.len() == limit {
                    return Ok(recent);
                }
                recent.push(entry);
            }
        }
        Ok(recent)
    }
}
