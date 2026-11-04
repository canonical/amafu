use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use amafu_domain::{ObjectReference, ProjectName};
use amafu_interfaces::{Consistency, Fence, Store, StoreError, StoredRecord, WatchEvent};
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::Value;
use tokio::sync::broadcast;

const CHANGES_KEPT_FOR_SLOW_WATCHERS: usize = 16_384;
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
    changes: BTreeMap<String, broadcast::Sender<WatchEvent>>,
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

fn differences(kind: &str, kept: &KindRecords, loaded: &KindRecords) -> Vec<ObjectReference> {
    let changed = loaded
        .iter()
        .filter(|(key, record)| kept.get(*key).map(|kept| kept.revision) != Some(record.revision));
    let removed = kept.iter().filter(|(key, _)| !loaded.contains_key(*key));
    changed.chain(removed).filter_map(|(_, record)| reference_of(kind, record)).collect()
}

async fn reload(
    inner: &dyn Store,
    replica: &RwLock<Replica>,
    kind: &str,
    changes: &broadcast::Sender<WatchEvent>,
) -> Result<(), StoreError> {
    let loaded = load(inner, kind).await?;
    let differing = {
        let mut replica = replica.write().unwrap_or_else(PoisonError::into_inner);
        let differing = replica.kinds.get(kind).map(|kept| differences(kind, kept, &loaded));
        replica.kinds.insert(kind.to_owned(), loaded);
        differing
    };
    match differing {
        Some(references) => {
            for reference in references {
                let _ = changes.send(WatchEvent::Changed(reference));
            }
        }
        None => {
            let _ = changes.send(WatchEvent::Resynchronize);
        }
    }
    Ok(())
}

async fn follow(
    inner: Arc<dyn Store>,
    replica: Arc<RwLock<Replica>>,
    kind: String,
    changes: broadcast::Sender<WatchEvent>,
) {
    let mut events = inner.watch(&kind);
    let mut is_current = false;
    loop {
        if !is_current {
            match reload(inner.as_ref(), &replica, &kind, &changes).await {
                Ok(()) => is_current = true,
                Err(error) => {
                    tracing::warn!(%kind, %error, "the read replica cannot load a kind yet");
                    tokio::time::sleep(RELOAD_DELAY).await;
                    continue;
                }
            }
        }
        match events.next().await {
            Some(WatchEvent::Changed(reference)) => match refresh(inner.as_ref(), &replica, &reference).await
            {
                Ok(()) => {
                    let _ = changes.send(WatchEvent::Changed(reference));
                }
                Err(error) => {
                    tracing::warn!(%kind, %error, "the read replica missed a change and reloads the kind");
                    is_current = false;
                }
            },
            Some(WatchEvent::Resynchronize) => is_current = false,
            None => {
                tokio::time::sleep(RELOAD_DELAY).await;
                events = inner.watch(&kind);
                is_current = false;
            }
        }
    }
}

fn shared_changes(changes: broadcast::Receiver<WatchEvent>) -> BoxStream<'static, WatchEvent> {
    futures::stream::unfold(changes, |mut changes| async move {
        match changes.recv().await {
            Ok(event) => Some((event, changes)),
            Err(broadcast::error::RecvError::Lagged(_)) => Some((WatchEvent::Resynchronize, changes)),
            Err(broadcast::error::RecvError::Closed) => None,
        }
    })
    .boxed()
}

impl ReadReplicaStore {
    #[must_use]
    pub fn start(inner: Arc<dyn Store>, kinds: &BTreeSet<&'static str>) -> Self {
        let replica = Arc::new(RwLock::new(Replica::default()));
        let mut changes = BTreeMap::new();
        for kind in kinds {
            let (sender, _) = broadcast::channel(CHANGES_KEPT_FOR_SLOW_WATCHERS);
            tokio::spawn(follow(
                Arc::clone(&inner),
                Arc::clone(&replica),
                (*kind).to_owned(),
                sender.clone(),
            ));
            changes.insert((*kind).to_owned(), sender);
        }
        Self { inner, replica, changes }
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
        match self.changes.get(kind) {
            Some(changes) => shared_changes(changes.subscribe()),
            None => self.inner.watch(kind),
        }
    }
}
