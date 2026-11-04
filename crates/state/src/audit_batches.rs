use std::sync::Arc;

use amafu_interfaces::{AuditEntry, AuditLog, StoreError};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::audit::QuoriumAuditLog;

const LARGEST_BATCH: usize = 4_096;
const QUEUE_LENGTH: usize = 32_768;

type Pending = (AuditEntry, oneshot::Sender<Result<(), StoreError>>);

pub struct BatchedAuditLog {
    log: Arc<QuoriumAuditLog>,
    queue: mpsc::Sender<Pending>,
}

async fn write_batches(log: Arc<QuoriumAuditLog>, mut queue: mpsc::Receiver<Pending>) {
    while let Some(first) = queue.recv().await {
        let mut batch = vec![first];
        while batch.len() < LARGEST_BATCH {
            match queue.try_recv() {
                Ok(next) => batch.push(next),
                Err(_) => break,
            }
        }
        let (entries, waiting): (Vec<AuditEntry>, Vec<oneshot::Sender<Result<(), StoreError>>>) =
            batch.into_iter().unzip();
        let outcome = log.record_many(&entries).await;
        if let Err(error) = &outcome {
            tracing::error!(%error, entries = entries.len(), "recording a batch of audit entries failed");
        }
        for waiter in waiting {
            let _ = waiter.send(outcome.clone());
        }
    }
}

impl BatchedAuditLog {
    #[must_use]
    pub fn start(log: QuoriumAuditLog) -> Self {
        let log = Arc::new(log);
        let (queue, received) = mpsc::channel(QUEUE_LENGTH);
        tokio::spawn(write_batches(Arc::clone(&log), received));
        Self { log, queue }
    }
}

#[async_trait::async_trait]
impl AuditLog for BatchedAuditLog {
    async fn record(&self, entry: AuditEntry) -> Result<(), StoreError> {
        let (answer, answered) = oneshot::channel();
        self.queue
            .send((entry, answer))
            .await
            .map_err(|_| StoreError::Unavailable(String::from("the audit writer stopped")))?;
        answered.await.map_err(|_| StoreError::Unavailable(String::from("the audit writer stopped")))?
    }

    async fn record_without_waiting(&self, entry: AuditEntry) -> Result<(), StoreError> {
        let (answer, _) = oneshot::channel();
        self.queue
            .try_send((entry, answer))
            .map_err(|_| StoreError::Unavailable(String::from("the audit queue is full")))
    }

    async fn find(&self, request_id: Uuid) -> Result<Option<AuditEntry>, StoreError> {
        self.log.find(request_id).await
    }

    async fn recent(&self, limit: usize) -> Result<Vec<AuditEntry>, StoreError> {
        self.log.recent(limit).await
    }
}
