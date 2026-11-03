use std::sync::Arc;
use std::time::Duration;

use amafu_domain::kinds::node::{Node, NodePhase, NodeStatus};
use amafu_domain::{Object, ObjectReference};
use amafu_interfaces::objects::{get, list, update_status};
use amafu_interfaces::{ActionError, Clock, Consistency, Store};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};
use time::OffsetDateTime;

pub struct NodeHealthController {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    silence_allowed: Duration,
}

impl NodeHealthController {
    #[must_use]
    pub fn new(store: Arc<dyn Store>, clock: Arc<dyn Clock>, silence_allowed: Duration) -> Self {
        Self { store, clock, silence_allowed }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodeHealthObservation {
    pub node: Option<Object<Node>>,
    pub now: OffsetDateTime,
    pub silence_allowed: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeHealthAction {
    MarkNotReady,
}

fn is_silent(status: &NodeStatus, now: OffsetDateTime, silence_allowed: Duration) -> bool {
    status.last_heartbeat.is_some_and(|heartbeat| now - heartbeat > silence_allowed)
}

#[must_use]
pub fn plan_node_health(observation: &NodeHealthObservation) -> Vec<NodeHealthAction> {
    match &observation.node {
        Some(node)
            if node.status.phase == NodePhase::Ready
                && is_silent(&node.status, observation.now, observation.silence_allowed) =>
        {
            vec![NodeHealthAction::MarkNotReady]
        }
        _ => Vec::new(),
    }
}

impl Controller for NodeHealthController {
    type Key = ObjectReference;
    type Observation = NodeHealthObservation;
    type Action = NodeHealthAction;

    const NAME: &'static str = "node-health";
    const WATCHES: &'static [WatchedKind<ObjectReference>] =
        &[WatchedKind { kind: "Node", mapping: KeyMapping::ObjectKey(|reference| Some(reference.clone())) }];

    async fn discover_keys(&self) -> Result<Vec<ObjectReference>, ActionError> {
        let nodes = list::<Node>(self.store.as_ref(), None, Consistency::Linearizable).await?;
        Ok(nodes.iter().map(Object::reference).collect())
    }

    async fn observe(&self, key: &ObjectReference) -> Result<NodeHealthObservation, ActionError> {
        Ok(NodeHealthObservation {
            node: get::<Node>(self.store.as_ref(), key, Consistency::Linearizable).await?,
            now: self.clock.now(),
            silence_allowed: self.silence_allowed,
        })
    }

    fn plan(&self, observation: &NodeHealthObservation) -> Vec<NodeHealthAction> {
        plan_node_health(observation)
    }

    async fn apply(&self, key: &ObjectReference, action: NodeHealthAction) -> Result<(), ActionError> {
        let NodeHealthAction::MarkNotReady = action;
        let (now, silence_allowed) = (self.clock.now(), self.silence_allowed);
        update_status::<Node>(self.store.as_ref(), key, |node| {
            is_silent(&node.status, now, silence_allowed)
                .then(|| NodeStatus { phase: NodePhase::NotReady, ..node.status.clone() })
        })
        .await?;
        Ok(())
    }

    fn revisit_after(&self, observation: &NodeHealthObservation) -> Option<Duration> {
        observation.node.as_ref().map(|_| self.silence_allowed / 2)
    }
}

#[cfg(test)]
mod tests {
    use amafu_domain::kinds::node::NodeSpecification;

    use super::*;
    use crate::testing::metadata;

    fn at(seconds: u64) -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn observation(phase: NodePhase, heartbeat: u64, now: u64) -> NodeHealthObservation {
        NodeHealthObservation {
            node: Some(Object {
                metadata: metadata("node-1", None),
                specification: NodeSpecification::default(),
                status: NodeStatus { phase, last_heartbeat: Some(at(heartbeat)), ..NodeStatus::default() },
            }),
            now: at(now),
            silence_allowed: Duration::from_secs(30),
        }
    }

    #[test]
    fn a_silent_ready_node_is_marked_not_ready() {
        assert_eq!(
            plan_node_health(&observation(NodePhase::Ready, 0, 31)),
            vec![NodeHealthAction::MarkNotReady]
        );
        assert_eq!(plan_node_health(&observation(NodePhase::Ready, 0, 30)), Vec::new());
        assert_eq!(plan_node_health(&observation(NodePhase::NotReady, 0, 300)), Vec::new());
    }
}
