use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use amafu_domain::{ObjectReference, ProjectName};
use amafu_interfaces::{Consistency, Fence, Store, StoreError, StoredRecord, WatchEvent};
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::Value;

const RELOAD_DELAY: Duration = Duration::from_secs(1);

type RecordKey = (String, String);
type KindRecords = BTreeMap<RecordKey, StoredRecord>;

#[derive(Default)]
struct Replica {
    kinds: BTreeMap<String, KindRecords>,
}

pub struct ReadReplicaStore {
    inner: Arc<dyn Store>,
    replica: Arc<RwLock<Replica>>,
}

fn record_key(reference: &ObjectReference) -> RecordKey {
    (reference.project.as_ref().map(ToString::to_string).unwrap_or_default(), reference.name.to_string())
}

fn reference_of(kind: &str, record: &StoredRecord) -> Option<ObjectReference> {
    serde_json::from_value::<amafu_domain::Metadata>(record.object.get("metadata")?.clone()).ok().map(
        |metadata| ObjectReference { kind: kind.to_owned(), project: metadata.project, name: metadata.name },
    )
}

async fn load(inner: &dyn Store, kind: &str) -> Result<KindRecords, StoreError> {
    let records = inner.list(kind, None, Consistency::Stale).await?;
    Ok(records
        .into_iter()
        .filter_map(|record| Some((record_key(&reference_of(kind, &record)?), record)))
        .collect())
}

async fn refresh(
    inner: &dyn Store,
    replica: &RwLock<Replica>,
    reference: &ObjectReference,
) -> Result<(), StoreError> {
    let current = inner.read(reference, Consistency::Stale).await?;
    let mut replica = replica.write().unwrap_or_else(PoisonError::into_inner);
    let Some(records) = replica.kinds.get_mut(&reference.kind) else {
        return Ok(());
    };
    let key = record_key(reference);
    match current {
        Some(record) if records.get(&key).is_none_or(|kept| kept.revision <= record.revision) => {
            records.insert(key, record);
        }
        Some(_) => {}
        None => {
            records.remove(&key);
        }
    }
    Ok(())
}

async fn follow(inner: Arc<dyn Store>, replica: Arc<RwLock<Replica>>, kind: String) {
    loop {
        let mut events = inner.watch(&kind);
        match load(inner.as_ref(), &kind).await {
            Ok(records) => {
                replica.write().unwrap_or_else(PoisonError::into_inner).kinds.insert(kind.clone(), records);
            }
            Err(error) => {
                tracing::warn!(%kind, %error, "the read replica cannot load a kind yet");
                tokio::time::sleep(RELOAD_DELAY).await;
                continue;
            }
        }
        while let Some(event) = events.next().await {
            let outcome = match event {
                WatchEvent::Changed(reference) => refresh(inner.as_ref(), &replica, &reference).await,
                WatchEvent::Resynchronize => break,
            };
            if let Err(error) = outcome {
                tracing::warn!(%kind, %error, "the read replica missed a change and reloads the kind");
                break;
            }
        }
        replica.write().unwrap_or_else(PoisonError::into_inner).kinds.remove(&kind);
        tokio::time::sleep(RELOAD_DELAY).await;
    }
}

impl ReadReplicaStore {
    #[must_use]
    pub fn start(inner: Arc<dyn Store>, kinds: &BTreeSet<&'static str>) -> Self {
        let replica = Arc::new(RwLock::new(Replica::default()));
        for kind in kinds {
            tokio::spawn(follow(Arc::clone(&inner), Arc::clone(&replica), (*kind).to_owned()));
        }
        Self { inner, replica }
    }

    fn replica(&self) -> std::sync::RwLockReadGuard<'_, Replica> {
        self.replica.read().unwrap_or_else(PoisonError::into_inner)
    }
}

#[async_trait::async_trait]
impl Store for ReadReplicaStore {
    async fn read(
        &self,
        reference: &ObjectReference,
        consistency: Consistency,
    ) -> Result<Option<StoredRecord>, StoreError> {
        if consistency == Consistency::Stale
            && let Some(records) = self.replica().kinds.get(&reference.kind)
        {
            return Ok(records.get(&record_key(reference)).cloned());
        }
        self.inner.read(reference, consistency).await
    }

    async fn list(
        &self,
        kind: &str,
        project: Option<&ProjectName>,
        consistency: Consistency,
    ) -> Result<Vec<StoredRecord>, StoreError> {
        if consistency == Consistency::Stale
            && let Some(records) = self.replica().kinds.get(kind)
        {
            let wanted = project.map(ToString::to_string);
            return Ok(records
                .iter()
                .filter(|((record_project, _), _)| {
                    wanted.as_ref().is_none_or(|wanted| wanted == record_project)
                })
                .map(|(_, record)| record.clone())
                .collect());
        }
        self.inner.list(kind, project, consistency).await
    }

    async fn compare_and_swap(
        &self,
        reference: &ObjectReference,
        expected_revision: Option<u64>,
        replacement: Option<Value>,
    ) -> Result<u64, StoreError> {
        self.inner.compare_and_swap(reference, expected_revision, replacement).await
    }

    async fn compare_and_swap_fenced(
        &self,
        reference: &ObjectReference,
        expected_revision: Option<u64>,
        replacement: Option<Value>,
        fence: &Fence,
    ) -> Result<u64, StoreError> {
        self.inner.compare_and_swap_fenced(reference, expected_revision, replacement, fence).await
    }

    fn watch(&self, kind: &str) -> BoxStream<'static, WatchEvent> {
        self.inner.watch(kind)
    }
}
