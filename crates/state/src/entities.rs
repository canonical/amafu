use std::collections::{BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

pub const RECENT_CHANGE_CAPACITY: usize = 256;
pub const AUDIT_SEGMENT_CAPACITY: u64 = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("the stored revision differs from the expected one")]
pub struct RevisionMismatch;

#[derive(quorium::State, Debug, Clone, Default, Serialize, Deserialize)]
#[state(key = (String, String, String), name = "amafu.object")]
pub struct ObjectEntity {
    revision: u64,
    object: Option<Vec<u8>>,
}

#[quorium::state]
impl ObjectEntity {
    #[quorium(id = 1)]
    pub fn compare_and_swap(
        &mut self,
        expected_revision: Option<u64>,
        replacement: Option<Vec<u8>>,
    ) -> Result<u64, RevisionMismatch> {
        let current_revision = self.object.as_ref().map(|_| self.revision);
        if current_revision != expected_revision {
            return Err(RevisionMismatch);
        }
        self.revision += 1;
        self.object = replacement;
        Ok(self.revision)
    }

    #[must_use]
    pub fn record(&self) -> Option<(u64, Vec<u8>)> {
        self.object.as_ref().map(|object| (self.revision, object.clone()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub sequence: u64,
    pub project: String,
    pub name: String,
}

#[derive(quorium::State, Debug, Clone, Default, Serialize, Deserialize)]
#[state(key = String, name = "amafu.kind-catalog")]
pub struct KindCatalog {
    members: BTreeSet<(String, String)>,
    sequence: u64,
    recent: VecDeque<Change>,
}

impl KindCatalog {
    fn note_change(&mut self, project: String, name: String) {
        self.sequence += 1;
        if self.recent.len() == RECENT_CHANGE_CAPACITY {
            self.recent.pop_front();
        }
        self.recent.push_back(Change { sequence: self.sequence, project, name });
    }

    #[must_use]
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    #[must_use]
    pub fn recent_changes(&self) -> Vec<Change> {
        self.recent.iter().cloned().collect()
    }

    #[must_use]
    pub fn changes_after(&self, sequence: u64) -> Option<Vec<Change>> {
        let oldest_kept = self.recent.front().map_or(self.sequence + 1, |change| change.sequence);
        if sequence + 1 < oldest_kept {
            return None;
        }
        Some(self.recent.iter().filter(|change| change.sequence > sequence).cloned().collect())
    }
}

#[quorium::state]
impl KindCatalog {
    #[quorium(id = 1)]
    pub fn add_member(&mut self, project: String, name: String) {
        self.members.insert((project, name));
    }

    #[quorium(id = 2)]
    pub fn record_change(&mut self, project: String, name: String) {
        self.note_change(project, name);
    }

    #[quorium(id = 3)]
    pub fn remove_member(&mut self, project: String, name: String) {
        self.members.remove(&(project.clone(), name.clone()));
        self.note_change(project, name);
    }

    #[must_use]
    pub fn members(&self) -> Vec<(String, String)> {
        self.members.iter().cloned().collect()
    }
}

#[derive(quorium::State, Debug, Clone, Default, Serialize, Deserialize)]
#[state(key = (), name = "amafu.audit-cursor")]
pub struct AuditCursor {
    next_sequence: u64,
}

#[quorium::state]
impl AuditCursor {
    #[quorium(id = 1)]
    pub fn allocate(&mut self) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        sequence
    }

    #[must_use]
    pub fn allocated(&self) -> u64 {
        self.next_sequence
    }
}

#[derive(quorium::State, Debug, Clone, Default, Serialize, Deserialize)]
#[state(key = u64, name = "amafu.audit-segment")]
pub struct AuditSegment {
    entries: Vec<(u64, Vec<u8>)>,
}

#[quorium::state]
impl AuditSegment {
    #[quorium(id = 1)]
    pub fn append(&mut self, sequence: u64, entry: Vec<u8>) {
        if !self.entries.iter().any(|(existing, _)| *existing == sequence) {
            self.entries.push((sequence, entry));
        }
    }

    #[must_use]
    pub fn entries(&self) -> Vec<(u64, Vec<u8>)> {
        self.entries.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog_with(changes: u64) -> KindCatalog {
        let mut catalog = KindCatalog::default();
        for index in 0..changes {
            catalog.note_change(String::from("project-a"), format!("vm-{index}"));
        }
        catalog
    }

    #[test]
    fn changes_after_a_known_sequence_are_listed_in_order() {
        let catalog = catalog_with(5);
        let names: Vec<String> =
            catalog.changes_after(3).unwrap().into_iter().map(|change| change.name).collect();
        assert_eq!(names, vec![String::from("vm-3"), String::from("vm-4")]);
        assert_eq!(catalog.changes_after(5), Some(Vec::new()));
    }

    #[test]
    fn a_watcher_that_fell_too_far_behind_must_resynchronize() {
        let catalog = catalog_with(RECENT_CHANGE_CAPACITY as u64 + 10);
        assert_eq!(catalog.changes_after(0), None);
        assert_eq!(catalog.changes_after(9), None);
        assert!(catalog.changes_after(10).is_some());
        assert_eq!(catalog.recent_changes().len(), RECENT_CHANGE_CAPACITY);
    }

    #[test]
    fn a_swap_succeeds_only_at_the_expected_revision() {
        let mut entity = ObjectEntity::default();
        assert_eq!(entity.compare_and_swap(Some(1), Some(vec![1])), Err(RevisionMismatch));
        assert_eq!(entity.compare_and_swap(None, Some(vec![1])), Ok(1));
        assert_eq!(entity.compare_and_swap(None, Some(vec![2])), Err(RevisionMismatch));
        assert_eq!(entity.compare_and_swap(Some(1), None), Ok(2));
        assert_eq!(entity.record(), None);
        assert_eq!(entity.compare_and_swap(None, Some(vec![3])), Ok(3));
    }
}
