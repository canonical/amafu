use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use amafu_domain::{ObjectReference, ProjectName};
use amafu_interfaces::{
    Consistency, CoreConnection, Fence, SliceChange, SliceRecord, Store, StoreError, StoredRecord, WatchEvent,
};
use futures::channel::mpsc::{UnboundedSender, unbounded};
use futures::stream::BoxStream;
use futures::{FutureExt, StreamExt};
use serde_json::Value;

#[derive(Debug, Default)]
pub struct LocalStateCache {
    path: Option<PathBuf>,
    records: BTreeMap<ObjectReference, StoredRecord>,
}

impl LocalStateCache {
    #[must_use]
    pub fn in_memory() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn on_disk(path: &Path) -> Self {
        let records = std::fs::read(path)
            .ok()
            .and_then(|contents| serde_json::from_slice::<Vec<SliceRecord>>(&contents).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|entry| (entry.reference, entry.record))
            .collect();
        Self { path: Some(path.to_path_buf()), records }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    fn apply(&mut self, change: SliceChange) -> Vec<WatchEvent> {
        match change {
            SliceChange::Snapshot { records } => {
                self.records = records.into_iter().map(|entry| (entry.reference, entry.record)).collect();
                vec![WatchEvent::Resynchronize]
            }
            SliceChange::Upserted { record } => {
                let reference = record.reference.clone();
                self.records.insert(record.reference, record.record);
                vec![WatchEvent::Changed(reference)]
            }
            SliceChange::Removed { reference } => {
                self.records.remove(&reference);
                vec![WatchEvent::Changed(reference)]
            }
            SliceChange::Heartbeat => Vec::new(),
        }
    }

    fn persist(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let entries: Vec<SliceRecord> = self
            .records
            .iter()
            .map(|(reference, record)| SliceRecord { reference: reference.clone(), record: record.clone() })
            .collect();
        let contents = serde_json::to_vec(&entries).map_err(std::io::Error::other)?;
        let partial = path.with_extension("partial");
        let mut file = std::fs::File::create(&partial)?;
        file.write_all(&contents)?;
        file.sync_all()?;
        std::fs::rename(&partial, path)?;
        if let Some(directory) = path.parent() {
            std::fs::File::open(directory)?.sync_all()?;
        }
        Ok(())
    }

    fn read(&self, reference: &ObjectReference) -> Option<StoredRecord> {
        self.records.get(reference).cloned()
    }

    fn list(&self, kind: &str, project: Option<&ProjectName>) -> Vec<StoredRecord> {
        self.records
            .iter()
            .filter(|(reference, _)| {
                reference.kind == kind
                    && project.is_none_or(|project| reference.project.as_ref() == Some(project))
            })
            .map(|(_, record)| record.clone())
            .collect()
    }
}

type Watchers = Vec<(String, UnboundedSender<WatchEvent>)>;

#[derive(Clone)]
pub struct SubscribedStore {
    core: Arc<dyn CoreConnection>,
    cache: Arc<Mutex<LocalStateCache>>,
    watchers: Arc<Mutex<Watchers>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl SubscribedStore {
    #[must_use]
    pub fn new(core: Arc<dyn CoreConnection>, cache: LocalStateCache) -> Self {
        Self { core, cache: Arc::new(Mutex::new(cache)), watchers: Arc::new(Mutex::new(Vec::new())) }
    }

    #[must_use]
    pub fn cached_object_count(&self) -> usize {
        lock(&self.cache).len()
    }

    fn notify(&self, events: Vec<WatchEvent>) {
        let mut watchers = lock(&self.watchers);
        for event in events {
            watchers.retain(|(kind, sender)| match &event {
                WatchEvent::Changed(reference) if reference.kind != *kind => true,
                _ => sender.unbounded_send(event.clone()).is_ok(),
            });
        }
    }

    fn apply(&self, changes: Vec<SliceChange>) {
        let mut events = Vec::new();
        {
            let mut cache = lock(&self.cache);
            for change in changes {
                events.extend(cache.apply(change));
            }
            if !events.is_empty()
                && let Err(error) = cache.persist()
            {
                tracing::warn!(%error, "the local state cache cannot be written");
            }
        }
        self.notify(events);
    }

    pub async fn follow(&self) -> StoreError {
        let mut changes = match self.core.subscribe().await {
            Ok(changes) => changes,
            Err(error) => return error,
        };
        while let Some(change) = changes.next().await {
            let mut batch = Vec::new();
            match change {
                Ok(change) => batch.push(change),
                Err(error) => return error,
            }
            while let Some(Some(Ok(change))) = changes.next().now_or_never() {
                batch.push(change);
            }
            self.apply(batch);
        }
        StoreError::Unavailable(String::from("the subscription ended"))
    }
}

#[async_trait::async_trait]
impl Store for SubscribedStore {
    async fn read(
        &self,
        reference: &ObjectReference,
        consistency: Consistency,
    ) -> Result<Option<StoredRecord>, StoreError> {
        match consistency {
            Consistency::Linearizable => self.core.read(reference).await,
            Consistency::Stale => Ok(lock(&self.cache).read(reference)),
        }
    }

    async fn list(
        &self,
        kind: &str,
        project: Option<&ProjectName>,
        consistency: Consistency,
    ) -> Result<Vec<StoredRecord>, StoreError> {
        match consistency {
            Consistency::Linearizable => self.core.list(kind, project).await,
            Consistency::Stale => Ok(lock(&self.cache).list(kind, project)),
        }
    }

    async fn compare_and_swap(
        &self,
        reference: &ObjectReference,
        expected_revision: Option<u64>,
        replacement: Option<Value>,
    ) -> Result<u64, StoreError> {
        self.core.compare_and_swap(reference, expected_revision, replacement).await
    }

    async fn compare_and_swap_fenced(
        &self,
        _reference: &ObjectReference,
        _expected_revision: Option<u64>,
        _replacement: Option<Value>,
        _fence: &Fence,
    ) -> Result<u64, StoreError> {
        Err(StoreError::Refused(String::from("only core members hold leases and write with fencing tokens")))
    }

    fn watch(&self, kind: &str) -> BoxStream<'static, WatchEvent> {
        let (sender, receiver) = unbounded();
        lock(&self.watchers).push((kind.to_owned(), sender));
        receiver.boxed()
    }
}

#[cfg(test)]
mod tests {
    use amafu_domain::ObjectName;
    use amafu_interfaces::SliceChanges;
    use serde_json::json;

    use super::*;

    fn reference(name: &str) -> ObjectReference {
        ObjectReference { kind: String::from("Vm"), project: None, name: ObjectName::new(name).unwrap() }
    }

    fn record(name: &str, revision: u64) -> SliceRecord {
        SliceRecord {
            reference: reference(name),
            record: StoredRecord { revision, object: json!({"name": name}) },
        }
    }

    struct ScriptedCore {
        changes: Mutex<Option<Vec<SliceChange>>>,
    }

    #[async_trait::async_trait]
    impl CoreConnection for ScriptedCore {
        async fn read(&self, _reference: &ObjectReference) -> Result<Option<StoredRecord>, StoreError> {
            Err(StoreError::Unavailable(String::from("the core is unreachable")))
        }

        async fn list(
            &self,
            _kind: &str,
            _project: Option<&ProjectName>,
        ) -> Result<Vec<StoredRecord>, StoreError> {
            Err(StoreError::Unavailable(String::from("the core is unreachable")))
        }

        async fn compare_and_swap(
            &self,
            _reference: &ObjectReference,
            _expected_revision: Option<u64>,
            _replacement: Option<Value>,
        ) -> Result<u64, StoreError> {
            Err(StoreError::Unavailable(String::from("the core is unreachable")))
        }

        async fn subscribe(&self) -> Result<SliceChanges, StoreError> {
            let changes = lock(&self.changes).take().unwrap_or_default();
            Ok(futures::stream::iter(changes.into_iter().map(Ok)).boxed())
        }
    }

    #[test]
    fn the_cache_follows_the_subscription_and_survives_a_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state-cache.json");
        let core = Arc::new(ScriptedCore {
            changes: Mutex::new(Some(vec![
                SliceChange::Snapshot { records: vec![record("web-1", 1), record("web-2", 2)] },
                SliceChange::Upserted { record: record("web-1", 3) },
                SliceChange::Removed { reference: reference("web-2") },
                SliceChange::Heartbeat,
            ])),
        });
        let store = SubscribedStore::new(core.clone(), LocalStateCache::on_disk(&path));
        let mut events = store.watch("Vm");
        let ended = futures::executor::block_on(store.follow());
        assert!(matches!(ended, StoreError::Unavailable(_)));
        let stale = futures::executor::block_on(store.list("Vm", None, Consistency::Stale)).unwrap();
        assert_eq!(stale.iter().map(|record| record.revision).collect::<Vec<_>>(), vec![3]);
        assert!(
            futures::executor::block_on(store.read(&reference("web-1"), Consistency::Linearizable)).is_err()
        );
        assert_eq!(futures::executor::block_on(events.next()), Some(WatchEvent::Resynchronize));
        let reopened = SubscribedStore::new(core, LocalStateCache::on_disk(&path));
        assert_eq!(reopened.cached_object_count(), 1);
    }

    #[test]
    fn nodes_outside_the_core_cannot_fence() {
        let store = SubscribedStore::new(
            Arc::new(ScriptedCore { changes: Mutex::new(None) }),
            LocalStateCache::in_memory(),
        );
        let fence = Fence { name: String::from("scheduler"), token: 1 };
        let refused = futures::executor::block_on(store.compare_and_swap_fenced(
            &reference("web-1"),
            None,
            None,
            &fence,
        ));
        assert!(matches!(refused, Err(StoreError::Refused(_))));
    }
}
