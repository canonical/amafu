use std::sync::Arc;

use amafu_domain::access::{Role, Verb};
use amafu_domain::kinds::audit_sink::AuditSinkSpecification;
use amafu_domain::{ObjectName, ProjectName};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::store::StoreError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum AuditOutcome {
    Unauthenticated,
    Denied,
    Rejected { status: u16 },
    Succeeded { status: u16, generation: Option<u64> },
}

impl AuditOutcome {
    #[must_use]
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Rejected { .. } | Self::Succeeded { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    pub request_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub recorded_at: OffsetDateTime,
    pub principal: Option<String>,
    pub role: Option<Role>,
    pub verb: Option<Verb>,
    pub kind: Option<String>,
    pub project: Option<ProjectName>,
    pub name: Option<ObjectName>,
    pub method: String,
    pub path: String,
    pub outcome: AuditOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequencedAuditEntry {
    pub sequence: u64,
    pub entry: AuditEntry,
}

#[async_trait::async_trait]
pub trait AuditSinkDelivery: Send + Sync {
    async fn deliver(&self, entries: &[SequencedAuditEntry]) -> Result<(), String>;
}

pub trait AuditSinks: Send + Sync {
    fn sink(&self, specification: &AuditSinkSpecification) -> Result<Arc<dyn AuditSinkDelivery>, String>;
}

#[async_trait::async_trait]
pub trait AuditLog: Send + Sync {
    async fn record(&self, entry: AuditEntry) -> Result<(), StoreError>;

    async fn record_without_waiting(&self, entry: AuditEntry) -> Result<(), StoreError> {
        self.record(entry).await
    }

    async fn find(&self, request_id: Uuid) -> Result<Option<AuditEntry>, StoreError>;

    async fn entries_from(&self, first: u64, limit: usize) -> Result<Vec<SequencedAuditEntry>, StoreError>;

    async fn recent(&self, limit: usize) -> Result<Vec<AuditEntry>, StoreError>;
}
