use std::sync::Arc;
use std::time::Duration;

use amafu_domain::kinds::audit_sink::{AuditSink, AuditSinkStatus};
use amafu_domain::{Object, ObjectReference};
use amafu_interfaces::objects::{get, list, remove, update_status_fenced};
use amafu_interfaces::{ActionError, AuditLog, AuditSinks, Consistency, Fence, SequencedAuditEntry, Store};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};

const ENTRIES_PER_DELIVERY: usize = 1_024;
const IDLE_REVISIT: Duration = Duration::from_secs(2);
const BEHIND_REVISIT: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, PartialEq)]
pub enum AuditExportObservation {
    Absent,
    Deleting,
    Current,
    Behind { sink: Box<Object<AuditSink>>, entries: Vec<SequencedAuditEntry> },
}

#[derive(Debug, Clone, PartialEq)]
pub enum AuditExportAction {
    Deliver { sink: Box<Object<AuditSink>>, entries: Vec<SequencedAuditEntry> },
    Remove,
}

#[must_use]
pub fn plan_audit_export(observation: &AuditExportObservation) -> Vec<AuditExportAction> {
    match observation {
        AuditExportObservation::Absent | AuditExportObservation::Current => Vec::new(),
        AuditExportObservation::Deleting => vec![AuditExportAction::Remove],
        AuditExportObservation::Behind { sink, entries } => {
            vec![AuditExportAction::Deliver { sink: sink.clone(), entries: entries.clone() }]
        }
    }
}

pub struct AuditExportController {
    store: Arc<dyn Store>,
    audit: Arc<dyn AuditLog>,
    sinks: Arc<dyn AuditSinks>,
    fence: Fence,
}

impl AuditExportController {
    #[must_use]
    pub fn new(
        store: Arc<dyn Store>,
        audit: Arc<dyn AuditLog>,
        sinks: Arc<dyn AuditSinks>,
        fence: Fence,
    ) -> Self {
        Self { store, audit, sinks, fence }
    }

    async fn record_status(
        &self,
        key: &ObjectReference,
        status: impl Fn(&AuditSinkStatus) -> AuditSinkStatus + Send + Sync,
    ) -> Result<(), ActionError> {
        update_status_fenced::<AuditSink>(self.store.as_ref(), key, &self.fence, |sink| {
            Some(status(&sink.status))
        })
        .await?;
        Ok(())
    }

    async fn deliver(
        &self,
        key: &ObjectReference,
        sink: &Object<AuditSink>,
        entries: &[SequencedAuditEntry],
    ) -> Result<(), ActionError> {
        let delivered = match self.sinks.sink(&sink.specification) {
            Ok(delivery) => delivery.deliver(entries).await,
            Err(failure) => Err(failure),
        };
        match (delivered, entries.last()) {
            (Ok(()), Some(last)) => {
                let last_sequence = last.sequence;
                self.record_status(key, |status| status.delivered_through(last_sequence)).await
            }
            (Ok(()), None) => Ok(()),
            (Err(failure), _) => {
                self.record_status(key, |status| status.failed(&failure)).await?;
                Err(ActionError::retryable(format!("exporting the audit log to {key}: {failure}")))
            }
        }
    }
}

impl Controller for AuditExportController {
    type Key = ObjectReference;
    type Observation = AuditExportObservation;
    type Action = AuditExportAction;

    const NAME: &'static str = "audit-export";
    const WATCHES: &'static [WatchedKind<ObjectReference>] = &[WatchedKind {
        kind: "AuditSink",
        mapping: KeyMapping::ObjectKey(|reference| Some(reference.clone())),
    }];

    async fn discover_keys(&self) -> Result<Vec<ObjectReference>, ActionError> {
        let sinks = list::<AuditSink>(self.store.as_ref(), None, Consistency::Linearizable).await?;
        Ok(sinks.iter().map(Object::reference).collect())
    }

    async fn observe(&self, key: &ObjectReference) -> Result<AuditExportObservation, ActionError> {
        let Some(sink) = get::<AuditSink>(self.store.as_ref(), key, Consistency::Linearizable).await? else {
            return Ok(AuditExportObservation::Absent);
        };
        if sink.metadata.is_deleting {
            return Ok(AuditExportObservation::Deleting);
        }
        let entries = self
            .audit
            .entries_from(sink.status.next_sequence, ENTRIES_PER_DELIVERY)
            .await
            .map_err(|error| ActionError::retryable(format!("reading the audit log: {error}")))?;
        if entries.is_empty() {
            return Ok(AuditExportObservation::Current);
        }
        Ok(AuditExportObservation::Behind { sink: Box::new(sink), entries })
    }

    fn plan(&self, observation: &AuditExportObservation) -> Vec<AuditExportAction> {
        plan_audit_export(observation)
    }

    async fn apply(&self, key: &ObjectReference, action: AuditExportAction) -> Result<(), ActionError> {
        match action {
            AuditExportAction::Deliver { sink, entries } => self.deliver(key, &sink, &entries).await,
            AuditExportAction::Remove => Ok(remove::<AuditSink>(self.store.as_ref(), key).await?),
        }
    }

    fn revisit_after(&self, observation: &AuditExportObservation) -> Option<Duration> {
        match observation {
            AuditExportObservation::Absent | AuditExportObservation::Deleting => None,
            AuditExportObservation::Current => Some(IDLE_REVISIT),
            AuditExportObservation::Behind { .. } => Some(BEHIND_REVISIT),
        }
    }
}
