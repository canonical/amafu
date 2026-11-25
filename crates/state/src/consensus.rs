use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use amafu_domain::kinds::node::StoreHealth;
use amafu_interfaces::{ActionError, ConsensusMembership};
use quorium::{Quorium, StepDown};

const STABLE_WINDOW: Duration = Duration::from_secs(30);
const HAND_OFF_DEADLINE: Duration = Duration::from_secs(4);
const STAND_ASIDE_WHILE_RESTARTING: Duration = Duration::from_secs(120);
const CLOSE_DEADLINE: Duration = Duration::from_secs(3);

#[derive(Default)]
struct SnapshotsSent {
    by_follower: BTreeMap<u64, (u64, Option<Instant>)>,
}

impl SnapshotsSent {
    fn followers_still_receiving(&mut self, sent: &BTreeMap<u64, u64>, now: Instant) -> u32 {
        for (follower, &count) in sent {
            let (seen, last_sent) = self.by_follower.entry(*follower).or_insert((count, None));
            if count > *seen {
                *seen = count;
                *last_sent = Some(now);
            }
        }
        let receiving = self
            .by_follower
            .values()
            .filter(|(_, last_sent)| last_sent.is_some_and(|at| now.duration_since(at) < STABLE_WINDOW))
            .count();
        u32::try_from(receiving).unwrap_or(u32::MAX)
    }
}

pub struct QuoriumMembership {
    node: Quorium,
    snapshots_sent: Mutex<SnapshotsSent>,
}

impl QuoriumMembership {
    #[must_use]
    pub fn new(node: Quorium) -> Self {
        Self { node, snapshots_sent: Mutex::default() }
    }
}

#[async_trait::async_trait]
impl ConsensusMembership for QuoriumMembership {
    fn health(&self) -> Option<StoreHealth> {
        let stats = self.node.stats();
        let followers_catching_up = self
            .snapshots_sent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .followers_still_receiving(&stats.snapshots_sent, Instant::now());
        Some(StoreHealth {
            is_stable: self.node.is_stable(STABLE_WINDOW),
            elections_started: stats.elections_started,
            leader_changes: stats.leader_changes,
            leaderless_seconds: stats.leaderless.as_secs(),
            followers_catching_up,
        })
    }

    async fn hand_off_leadership(&self) -> Result<(), ActionError> {
        let step_down =
            StepDown { deadline: HAND_OFF_DEADLINE, stand_aside: STAND_ASIDE_WHILE_RESTARTING, prefer: None };
        self.node.step_down(step_down).await.map(drop).map_err(ActionError::retryable)
    }

    async fn close(&self) {
        let report = self.node.clone().shutdown_gracefully(CLOSE_DEADLINE).await;
        if !report.is_clean() {
            tracing::warn!(?report, "the store did not close cleanly before the restart");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_follower_counts_as_catching_up_while_it_keeps_receiving_snapshots() {
        let mut snapshots = SnapshotsSent::default();
        let start = Instant::now();
        let sent = |count| BTreeMap::from([(7, count), (8, 1)]);
        assert_eq!(snapshots.followers_still_receiving(&sent(1), start), 0);
        assert_eq!(snapshots.followers_still_receiving(&sent(2), start + Duration::from_secs(1)), 1);
        assert_eq!(snapshots.followers_still_receiving(&sent(5), start + Duration::from_secs(20)), 1);
        let later = start + Duration::from_secs(20) + STABLE_WINDOW;
        assert_eq!(snapshots.followers_still_receiving(&sent(5), later), 0);
    }
}
