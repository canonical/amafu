use std::sync::Mutex;

use amafu_interfaces::{AuditEntry, AuditLog, StoreError};
use uuid::Uuid;

use crate::lock::lock;

#[derive(Default)]
pub struct MemoryAuditLog {
    entries: Mutex<Vec<AuditEntry>>,
}

impl MemoryAuditLog {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn entries(&self) -> Vec<AuditEntry> {
        lock(&self.entries).clone()
    }
}

#[async_trait::async_trait]
impl AuditLog for MemoryAuditLog {
    async fn record(&self, entry: AuditEntry) -> Result<(), StoreError> {
        lock(&self.entries).push(entry);
        Ok(())
    }

    async fn find(&self, request_id: Uuid) -> Result<Option<AuditEntry>, StoreError> {
        Ok(lock(&self.entries).iter().find(|entry| entry.request_id == request_id).cloned())
    }

    async fn recent(&self, limit: usize) -> Result<Vec<AuditEntry>, StoreError> {
        let entries = lock(&self.entries);
        Ok(entries.iter().rev().take(limit).cloned().collect())
    }
}
