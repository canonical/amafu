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

impl QuoriumAuditLog {
    pub async fn record_many(&self, entries: &[AuditEntry]) -> Result<(), StoreError> {
        let encoded = entries
            .iter()
            .map(|entry| serde_json::to_vec(entry).map_err(|error| StoreError::Corrupt(error.to_string())))
            .collect::<Result<Vec<_>, _>>()?;
        let count = u64::try_from(encoded.len()).unwrap_or(u64::MAX);
        let first = self.node.global::<AuditCursor>().allocate_many(count).await.map_err(store_error)?;
        let mut by_segment: std::collections::BTreeMap<u64, Vec<(u64, Vec<u8>)>> =
            std::collections::BTreeMap::new();
        for (offset, bytes) in (0..).zip(encoded) {
            let sequence = first + offset;
            by_segment.entry(sequence / AUDIT_SEGMENT_CAPACITY).or_default().push((sequence, bytes));
        }
        for (segment, entries) in by_segment {
            self.node.entity::<AuditSegment>(segment).append_many(entries).await.map_err(store_error)?;
        }
        Ok(())
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
