use std::collections::VecDeque;

use amafu_domain::{ObjectReference, ProjectName};
use amafu_interfaces::{Consistency, Store, StoreError, StoredRecord, WatchEvent};
use futures::StreamExt;
use futures::stream::BoxStream;
use quorium::{Quorium, Watch};
use serde_json::Value;

use crate::entities::{Change, KindCatalog, ObjectEntity};
use crate::errors::store_error;

#[derive(Clone)]
pub struct QuoriumStore {
    node: Quorium,
}

type ObjectKey = (String, String, String);

fn project_part(project: Option<&ProjectName>) -> String {
    project.map(ToString::to_string).unwrap_or_default()
}

fn object_key(reference: &ObjectReference) -> ObjectKey {
    (reference.kind.clone(), project_part(reference.project.as_ref()), reference.name.to_string())
}

fn decode(revision: u64, bytes: &[u8]) -> Result<StoredRecord, StoreError> {
    let object = serde_json::from_slice(bytes).map_err(|error| StoreError::Corrupt(error.to_string()))?;
    Ok(StoredRecord { revision, object })
}

fn encode(object: &Value) -> Result<Vec<u8>, StoreError> {
    serde_json::to_vec(object).map_err(|error| StoreError::Corrupt(error.to_string()))
}

impl QuoriumStore {
    #[must_use]
    pub fn new(node: Quorium) -> Self {
        Self { node }
    }

    async fn read_key(
        &self,
        key: ObjectKey,
        consistency: Consistency,
    ) -> Result<Option<StoredRecord>, StoreError> {
        let handle = self.node.entity::<ObjectEntity>(key);
        let record = match consistency {
            Consistency::Linearizable => handle.record().await,
            Consistency::Stale => handle.stale().record().await,
        }
        .map_err(store_error)?;
        record.map(|(revision, bytes)| decode(revision, &bytes)).transpose()
    }
}

#[async_trait::async_trait]
impl Store for QuoriumStore {
    async fn read(
        &self,
        reference: &ObjectReference,
        consistency: Consistency,
    ) -> Result<Option<StoredRecord>, StoreError> {
        self.read_key(object_key(reference), consistency).await
    }

    async fn list(
        &self,
        kind: &str,
        project: Option<&ProjectName>,
        consistency: Consistency,
    ) -> Result<Vec<StoredRecord>, StoreError> {
        let catalog = self.node.entity::<KindCatalog>(kind.to_owned());
        let members = match consistency {
            Consistency::Linearizable => catalog.members().await,
            Consistency::Stale => catalog.stale().members().await,
        }
        .map_err(store_error)?;
        let wanted_project = project.map(ToString::to_string);
        let mut records = Vec::new();
        for (member_project, name) in members {
            if wanted_project.as_ref().is_some_and(|wanted| *wanted != member_project) {
                continue;
            }
            if let Some(record) = self.read_key((kind.to_owned(), member_project, name), consistency).await? {
                records.push(record);
            }
        }
        Ok(records)
    }

    async fn compare_and_swap(
        &self,
        reference: &ObjectReference,
        expected_revision: Option<u64>,
        replacement: Option<Value>,
    ) -> Result<u64, StoreError> {
        let (kind, project, name) = object_key(reference);
        let catalog = self.node.entity::<KindCatalog>(kind.clone());
        let is_creation = expected_revision.is_none() && replacement.is_some();
        let is_removal = replacement.is_none();
        if is_creation {
            catalog.add_member(project.clone(), name.clone()).await.map_err(store_error)?;
        }
        let bytes = replacement.as_ref().map(encode).transpose()?;
        let revision = self
            .node
            .entity::<ObjectEntity>((kind, project.clone(), name.clone()))
            .compare_and_swap(expected_revision, bytes)
            .await
            .map_err(store_error)?;
        let noted = if is_removal {
            catalog.remove_member(project, name).await
        } else {
            catalog.record_change(project, name).await
        };
        if let Err(error) = noted {
            tracing::warn!(error = %store_error(error), "the change was stored but watchers were not told");
        }
        Ok(revision)
    }

    fn watch(&self, kind: &str) -> BoxStream<'static, WatchEvent> {
        let progress = CatalogWatch {
            watch: self.node.entity::<KindCatalog>(kind.to_owned()).watch(),
            kind: kind.to_owned(),
            last_sequence: None,
            pending: VecDeque::new(),
        };
        futures::stream::unfold(progress, |mut progress| async move {
            loop {
                if let Some(event) = progress.pending.pop_front() {
                    return Some((event, progress));
                }
                let catalog = progress.watch.changed().await.ok()?;
                progress.pending = progress.events_since_last(&catalog);
                progress.last_sequence = Some(catalog.sequence());
            }
        })
        .boxed()
    }
}

struct CatalogWatch {
    watch: Watch<KindCatalog>,
    kind: String,
    last_sequence: Option<u64>,
    pending: VecDeque<WatchEvent>,
}

impl CatalogWatch {
    fn events_since_last(&self, catalog: &KindCatalog) -> VecDeque<WatchEvent> {
        let Some(changes) = self.last_sequence.and_then(|last| catalog.changes_after(last)) else {
            let mut events = VecDeque::from([WatchEvent::Resynchronize]);
            events.extend(self.changed_events(&catalog.recent_changes()));
            return events;
        };
        self.changed_events(&changes).collect()
    }

    fn changed_events<'changes>(
        &'changes self,
        changes: &'changes [Change],
    ) -> impl Iterator<Item = WatchEvent> + 'changes {
        changes
            .iter()
            .filter_map(|change| change_reference(&self.kind, &change.project, &change.name))
            .map(WatchEvent::Changed)
    }
}

fn change_reference(kind: &str, project: &str, name: &str) -> Option<ObjectReference> {
    let project = if project.is_empty() { None } else { Some(ProjectName::new(project).ok()?) };
    Some(ObjectReference { kind: kind.to_owned(), project, name: name.parse().ok()? })
}
