use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use amafu_domain::{ObjectReference, ProjectName};
use amafu_interfaces::{Consistency, Fence, Store, StoreError, StoredRecord, WatchEvent};
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct JournalContents {
    revision: u64,
    records: BTreeMap<String, StoredRecord>,
}

pub struct JournalStore {
    path: PathBuf,
    contents: Mutex<JournalContents>,
}

fn key(reference: &ObjectReference) -> String {
    let project = reference.project.as_ref().map(ToString::to_string).unwrap_or_default();
    format!("{}/{project}/{}", reference.kind, reference.name)
}

fn unavailable(error: impl std::fmt::Display) -> StoreError {
    StoreError::Unavailable(error.to_string())
}

impl JournalStore {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let contents = match std::fs::read(path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|error| StoreError::Corrupt(error.to_string()))?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => JournalContents::default(),
            Err(error) => return Err(unavailable(error)),
        };
        Ok(Self { path: path.to_path_buf(), contents: Mutex::new(contents) })
    }

    fn contents(&self) -> MutexGuard<'_, JournalContents> {
        self.contents.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn persist(&self, contents: &JournalContents) -> Result<(), StoreError> {
        let temporary = self.path.with_extension("partial");
        let encoded =
            serde_json::to_vec_pretty(contents).map_err(|error| StoreError::Corrupt(error.to_string()))?;
        std::fs::write(&temporary, encoded).map_err(unavailable)?;
        std::fs::File::open(&temporary).and_then(|file| file.sync_all()).map_err(unavailable)?;
        std::fs::rename(&temporary, &self.path).map_err(unavailable)
    }
}

#[async_trait::async_trait]
impl Store for JournalStore {
    async fn read(
        &self,
        reference: &ObjectReference,
        _consistency: Consistency,
    ) -> Result<Option<StoredRecord>, StoreError> {
        Ok(self.contents().records.get(&key(reference)).cloned())
    }

    async fn list(
        &self,
        kind: &str,
        project: Option<&ProjectName>,
        _consistency: Consistency,
    ) -> Result<Vec<StoredRecord>, StoreError> {
        let prefix = match project {
            Some(project) => format!("{kind}/{project}/"),
            None => format!("{kind}/"),
        };
        Ok(self
            .contents()
            .records
            .iter()
            .filter(|(stored, _)| stored.starts_with(&prefix))
            .map(|(_, record)| record.clone())
            .collect())
    }

    async fn compare_and_swap(
        &self,
        reference: &ObjectReference,
        expected_revision: Option<u64>,
        replacement: Option<Value>,
    ) -> Result<u64, StoreError> {
        let mut contents = self.contents();
        let stored = key(reference);
        let current = contents.records.get(&stored).map(|record| record.revision);
        if current != expected_revision {
            return Err(StoreError::Conflict);
        }
        let mut changed = contents.clone();
        changed.revision += 1;
        let revision = changed.revision;
        match replacement {
            Some(object) => {
                changed.records.insert(stored, StoredRecord { revision, object });
            }
            None => {
                changed.records.remove(&stored);
            }
        }
        self.persist(&changed)?;
        *contents = changed;
        Ok(revision)
    }

    async fn compare_and_swap_fenced(
        &self,
        reference: &ObjectReference,
        expected_revision: Option<u64>,
        replacement: Option<Value>,
        _fence: &Fence,
    ) -> Result<u64, StoreError> {
        self.compare_and_swap(reference, expected_revision, replacement).await
    }

    fn watch(&self, _kind: &str) -> BoxStream<'static, WatchEvent> {
        Box::pin(futures::stream::pending())
    }
}

#[cfg(test)]
mod tests {
    use amafu_domain::ObjectName;
    use serde_json::json;

    use super::*;

    fn reference(name: &str) -> ObjectReference {
        ObjectReference {
            kind: String::from("Operation"),
            project: None,
            name: ObjectName::new(name).unwrap(),
        }
    }

    #[tokio::test]
    async fn records_survive_reopening_and_stale_writes_conflict() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("journal.json");
        let journal = JournalStore::open(&path).unwrap();
        let first =
            journal.compare_and_swap(&reference("install"), None, Some(json!({"step": 1}))).await.unwrap();
        assert_eq!(
            journal.compare_and_swap(&reference("install"), None, Some(json!({"step": 2}))).await,
            Err(StoreError::Conflict)
        );
        journal.compare_and_swap(&reference("install"), Some(first), Some(json!({"step": 2}))).await.unwrap();
        let reopened = JournalStore::open(&path).unwrap();
        let record = reopened.read(&reference("install"), Consistency::Linearizable).await.unwrap().unwrap();
        assert_eq!(record.object, json!({"step": 2}));
        assert_eq!(reopened.list("Operation", None, Consistency::Linearizable).await.unwrap().len(), 1);
    }
}
