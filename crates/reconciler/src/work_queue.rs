use std::collections::{BTreeMap, BTreeSet};

use time::OffsetDateTime;

#[derive(Debug, Clone)]
pub struct WorkQueue<Key: Ord + Clone> {
    ready_at: BTreeMap<Key, OffsetDateTime>,
    parked: BTreeSet<Key>,
    consecutive_failures: BTreeMap<Key, u32>,
}

impl<Key: Ord + Clone> Default for WorkQueue<Key> {
    fn default() -> Self {
        Self { ready_at: BTreeMap::new(), parked: BTreeSet::new(), consecutive_failures: BTreeMap::new() }
    }
}

impl<Key: Ord + Clone> WorkQueue<Key> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn notify_changed(&mut self, key: Key, now: OffsetDateTime) {
        self.parked.remove(&key);
        let ready_at = self.ready_at.entry(key).or_insert(now);
        *ready_at = (*ready_at).min(now);
    }

    pub fn schedule(&mut self, key: Key, at: OffsetDateTime) {
        if self.parked.contains(&key) {
            return;
        }
        let ready_at = self.ready_at.entry(key).or_insert(at);
        *ready_at = (*ready_at).min(at);
    }

    pub fn park_until_changed(&mut self, key: Key) {
        self.ready_at.remove(&key);
        self.parked.insert(key);
    }

    pub fn pop_ready(&mut self, now: OffsetDateTime) -> Option<Key> {
        let key = self
            .ready_at
            .iter()
            .filter(|(_, ready_at)| **ready_at <= now)
            .min_by(|left, right| left.1.cmp(right.1).then_with(|| left.0.cmp(right.0)))
            .map(|(key, _)| key.clone())?;
        self.ready_at.remove(&key);
        Some(key)
    }

    #[must_use]
    pub fn next_ready_at(&self) -> Option<OffsetDateTime> {
        self.ready_at.values().min().copied()
    }

    pub fn record_failure(&mut self, key: &Key) -> u32 {
        let failures = self.consecutive_failures.entry(key.clone()).or_insert(0);
        *failures = failures.saturating_add(1);
        *failures
    }

    pub fn record_success(&mut self, key: &Key) {
        self.consecutive_failures.remove(key);
    }

    #[must_use]
    pub fn is_waiting(&self, key: &Key) -> bool {
        self.ready_at.contains_key(key)
    }

    #[must_use]
    pub fn is_parked(&self, key: &Key) -> bool {
        self.parked.contains(key)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.ready_at.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ready_at.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use proptest::prelude::*;

    use super::*;

    fn at(seconds: u64) -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[test]
    fn keys_are_deduplicated() {
        let mut queue = WorkQueue::new();
        queue.notify_changed("web", at(0));
        queue.notify_changed("web", at(0));
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.pop_ready(at(0)), Some("web"));
        assert_eq!(queue.pop_ready(at(0)), None);
    }

    #[test]
    fn scheduled_keys_wait_for_their_time() {
        let mut queue = WorkQueue::new();
        queue.schedule("web", at(10));
        assert_eq!(queue.pop_ready(at(9)), None);
        assert_eq!(queue.next_ready_at(), Some(at(10)));
        assert_eq!(queue.pop_ready(at(10)), Some("web"));
    }

    #[test]
    fn a_change_overrides_a_later_schedule() {
        let mut queue = WorkQueue::new();
        queue.schedule("web", at(30));
        queue.notify_changed("web", at(1));
        assert_eq!(queue.pop_ready(at(1)), Some("web"));
    }

    #[test]
    fn parked_keys_return_only_on_change() {
        let mut queue = WorkQueue::new();
        queue.park_until_changed("web");
        queue.schedule("web", at(0));
        assert_eq!(queue.pop_ready(at(100)), None);
        assert!(queue.is_parked(&"web"));
        queue.notify_changed("web", at(100));
        assert_eq!(queue.pop_ready(at(100)), Some("web"));
    }

    #[test]
    fn the_earliest_key_comes_first_and_ties_break_by_key() {
        let mut queue = WorkQueue::new();
        queue.schedule("zeta", at(1));
        queue.schedule("beta", at(2));
        queue.schedule("alpha", at(2));
        assert_eq!(queue.pop_ready(at(5)), Some("zeta"));
        assert_eq!(queue.pop_ready(at(5)), Some("alpha"));
        assert_eq!(queue.pop_ready(at(5)), Some("beta"));
    }

    #[test]
    fn failures_accumulate_until_success() {
        let mut queue = WorkQueue::<&str>::new();
        assert_eq!(queue.record_failure(&"web"), 1);
        assert_eq!(queue.record_failure(&"web"), 2);
        queue.record_success(&"web");
        assert_eq!(queue.record_failure(&"web"), 1);
    }

    proptest! {
        #[test]
        fn every_notified_key_is_eventually_popped_once(keys in proptest::collection::vec(0_u8..16, 0..64)) {
            let mut queue = WorkQueue::new();
            for key in &keys {
                queue.notify_changed(*key, at(0));
            }
            let mut popped = Vec::new();
            while let Some(key) = queue.pop_ready(at(0)) {
                popped.push(key);
            }
            let expected: BTreeSet<u8> = keys.iter().copied().collect();
            prop_assert_eq!(popped.len(), expected.len());
            prop_assert_eq!(popped.into_iter().collect::<BTreeSet<u8>>(), expected);
        }
    }
}
