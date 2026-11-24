use std::time::Duration;

use amafu_domain::kinds::node::StoreHealth;
use amafu_interfaces::{ActionError, ConsensusMembership};
use quorium::{Quorium, StepDown};

const STABLE_WINDOW: Duration = Duration::from_secs(30);
const HAND_OFF_DEADLINE: Duration = Duration::from_secs(4);
const STAND_ASIDE_WHILE_RESTARTING: Duration = Duration::from_secs(120);
const CLOSE_DEADLINE: Duration = Duration::from_secs(3);

pub struct QuoriumMembership {
    node: Quorium,
}

impl QuoriumMembership {
    #[must_use]
    pub fn new(node: Quorium) -> Self {
        Self { node }
    }
}

#[async_trait::async_trait]
impl ConsensusMembership for QuoriumMembership {
    fn health(&self) -> Option<StoreHealth> {
        let stats = self.node.stats();
        Some(StoreHealth {
            is_stable: self.node.is_stable(STABLE_WINDOW),
            elections_started: stats.elections_started,
            leader_changes: stats.leader_changes,
            leaderless_seconds: stats.leaderless.as_secs(),
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
