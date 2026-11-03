use std::collections::BTreeMap;
use std::sync::Mutex;

use amafu_domain::{ObjectReference, ProjectName};
use amafu_interfaces::{Consistency, Fence, Store, StoreError, StoredRecord, WatchEvent};
use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use futures::stream::BoxStream;
use serde_json::Value;

use crate::lock::lock;

#[derive(Default)]
struct StoreState {
    records: BTreeMap<ObjectReference, StoredRecord>,
    revision: u64,
    watchers: Vec<(String, UnboundedSender<WatchEvent>)>,
    is_unavailable: bool,
    write_count: u64,
    fences: BTreeMap<(ObjectReference, String), u64>,
    fenced_writes: Vec<FencedWrite>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FencedWrite {
    pub reference: ObjectReference,
    pub fence: Fence,
    pub is_accepted: bool,
}

#[derive(Default)]
pub struct MemoryStore {
    state: Mutex<StoreState>,
}

impl MemoryStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_unavailable(&self, is_unavailable: bool) {
        lock(&self.state).is_unavailable = is_unavailable;
    }

    #[must_use]
    pub fn all_objects(&self) -> Vec<Value> {
        lock(&self.state).records.values().map(|record| record.object.clone()).collect()
    }

    #[must_use]
    pub fn write_count(&self) -> u64 {
        lock(&self.state).write_count
    }

    #[must_use]
    pub fn fenced_writes(&self) -> Vec<FencedWrite> {
        lock(&self.state).fenced_writes.clone()
    }

    fn check_available(state: &StoreState) -> Result<(), StoreError> {
        if state.is_unavailable {
            Err(StoreError::Unavailable(String::from("simulated outage")))
        } else {
            Ok(())
        }
    }
}

#[async_trait::async_trait]
impl Store for MemoryStore {
    async fn read(
        &self,
        reference: &ObjectReference,
        _consistency: Consistency,
    ) -> Result<Option<StoredRecord>, StoreError> {
        let state = lock(&self.state);
        Self::check_available(&state)?;
        Ok(state.records.get(reference).cloned())
    }

    async fn list(
        &self,
        kind: &str,
        project: Option<&ProjectName>,
        _consistency: Consistency,
    ) -> Result<Vec<StoredRecord>, StoreError> {
        let state = lock(&self.state);
        Self::check_available(&state)?;
        Ok(state
            .records
            .iter()
            .filter(|(reference, _)| {
                reference.kind == kind
                    && project.is_none_or(|project| reference.project.as_ref() == Some(project))
            })
            .map(|(_, record)| record.clone())
            .collect())
    }

    async fn compare_and_swap(
        &self,
        reference: &ObjectReference,
        expected_revision: Option<u64>,
        replacement: Option<Value>,
    ) -> Result<u64, StoreError> {
        let mut state = lock(&self.state);
        Self::check_available(&state)?;
        Self::swap(&mut state, reference, expected_revision, replacement)
    }

    async fn compare_and_swap_fenced(
        &self,
        reference: &ObjectReference,
        expected_revision: Option<u64>,
        replacement: Option<Value>,
        fence: &Fence,
    ) -> Result<u64, StoreError> {
        let mut state = lock(&self.state);
        Self::check_available(&state)?;
        let key = (reference.clone(), fence.name.clone());
        let highest = state.fences.get(&key).copied().unwrap_or(0);
        if fence.token < highest {
            state.fenced_writes.push(FencedWrite {
                reference: reference.clone(),
                fence: fence.clone(),
                is_accepted: false,
            });
            return Err(StoreError::Fenced { name: fence.name.clone(), token: fence.token });
        }
        let revision = Self::swap(&mut state, reference, expected_revision, replacement)?;
        state.fences.insert(key, fence.token);
        state.fenced_writes.push(FencedWrite {
            reference: reference.clone(),
            fence: fence.clone(),
            is_accepted: true,
        });
        Ok(revision)
    }

    fn watch(&self, kind: &str) -> BoxStream<'static, WatchEvent> {
        let (sender, receiver) = unbounded();
        lock(&self.state).watchers.push((kind.to_owned(), sender));
        receiver.boxed()
    }
}

impl MemoryStore {
    fn swap(
        state: &mut StoreState,
        reference: &ObjectReference,
        expected_revision: Option<u64>,
        replacement: Option<Value>,
    ) -> Result<u64, StoreError> {
        let current_revision = state.records.get(reference).map(|record| record.revision);
        if current_revision != expected_revision {
            return Err(StoreError::Conflict);
        }
        state.revision += 1;
        state.write_count += 1;
        let revision = state.revision;
        match replacement {
            Some(object) => {
                state.records.insert(reference.clone(), StoredRecord { revision, object });
            }
            None => {
                state.records.remove(reference);
            }
        }
        state.watchers.retain(|(kind, sender)| {
            kind != &reference.kind || sender.unbounded_send(WatchEvent::Changed(reference.clone())).is_ok()
        });
        Ok(revision)
    }
}

#[cfg(test)]
mod tests {
    use amafu_interfaces::contract::store::store_contract;

    use super::*;

    #[test]
    fn the_memory_store_honours_the_store_contract() {
        let store = MemoryStore::new();
        assert_eq!(futures::executor::block_on(store_contract(&store)), Ok(()));
    }
}
