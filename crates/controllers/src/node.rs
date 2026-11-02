use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use amafu_domain::kinds::node::{Node, NodeCapacity, NodePhase, NodeRole, NodeSpecification, NodeStatus};
use amafu_domain::{Change, NodeName, Object, ObjectId, ObjectName, ObjectReference};
use amafu_interfaces::objects::{apply, get, remove};
use amafu_interfaces::{ActionError, Clock, Consistency, Store};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeIdentity {
    pub name: NodeName,
    pub id: ObjectId,
    pub roles: std::collections::BTreeSet<NodeRole>,
    pub capacity: NodeCapacity,
    pub software_version: String,
    pub heartbeat_interval: Duration,
}

pub struct NodeController {
    identity: NodeIdentity,
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
}

impl NodeController {
    #[must_use]
    pub fn new(identity: NodeIdentity, store: Arc<dyn Store>, clock: Arc<dyn Clock>) -> Self {
        Self { identity, store, clock }
    }

    fn reference(&self) -> ObjectReference {
        node_reference(&self.identity.name)
    }
}

#[must_use]
pub fn node_reference(name: &NodeName) -> ObjectReference {
    ObjectReference {
        kind: String::from("Node"),
        project: None,
        name: ObjectName::new(name.as_str()).unwrap_or_else(|_| unreachable!()),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodeObservation {
    pub identity: NodeIdentity,
    pub node: Option<Object<Node>>,
    pub now: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeAction {
    Register(NodeSpecification),
    ReportReady { observed_generation: u64, status: NodeStatus },
    Remove,
}

fn is_heartbeat_due(status: &NodeStatus, now: OffsetDateTime, interval: Duration) -> bool {
    status.last_heartbeat.is_none_or(|heartbeat| now - heartbeat >= interval)
}

#[must_use]
pub fn plan_node(observation: &NodeObservation) -> Vec<NodeAction> {
    let identity = &observation.identity;
    let Some(node) = &observation.node else {
        return vec![NodeAction::Register(NodeSpecification {
            roles: identity.roles.clone(),
            is_cordoned: false,
        })];
    };
    if node.metadata.is_deleting {
        return vec![NodeAction::Remove];
    }
    let is_current = node.is_status_current()
        && node.status.phase == NodePhase::Ready
        && node.status.capacity == identity.capacity
        && node.status.software_version == identity.software_version
        && !is_heartbeat_due(&node.status, observation.now, identity.heartbeat_interval);
    if is_current {
        return Vec::new();
    }
    vec![NodeAction::ReportReady {
        observed_generation: node.metadata.generation,
        status: NodeStatus {
            phase: NodePhase::Ready,
            capacity: identity.capacity,
            software_version: identity.software_version.clone(),
            last_heartbeat: Some(observation.now),
        },
    }]
}

impl Controller for NodeController {
    type Key = NodeName;
    type Observation = NodeObservation;
    type Action = NodeAction;

    const NAME: &'static str = "node";
    const WATCHES: &'static [WatchedKind<NodeName>] = &[WatchedKind {
        kind: "Node",
        mapping: KeyMapping::ObjectKey(|reference| NodeName::new(reference.name.as_str()).ok()),
    }];

    async fn discover_keys(&self) -> Result<Vec<NodeName>, ActionError> {
        Ok(vec![self.identity.name.clone()])
    }

    async fn observe(&self, key: &NodeName) -> Result<NodeObservation, ActionError> {
        let node = if *key == self.identity.name {
            get::<Node>(self.store.as_ref(), &self.reference(), Consistency::Linearizable).await?
        } else {
            None
        };
        Ok(NodeObservation { identity: self.identity.clone(), node, now: self.clock.now() })
    }

    fn plan(&self, observation: &NodeObservation) -> Vec<NodeAction> {
        if observation
            .node
            .as_ref()
            .is_some_and(|node| node.metadata.name.as_str() != self.identity.name.as_str())
        {
            return Vec::new();
        }
        plan_node(observation)
    }

    async fn apply(&self, key: &NodeName, action: NodeAction) -> Result<(), ActionError> {
        if *key != self.identity.name {
            return Ok(());
        }
        let store = self.store.as_ref();
        let reference = self.reference();
        match action {
            NodeAction::Register(specification) => {
                let change = Change::Create {
                    id: self.identity.id,
                    name: reference.name.clone(),
                    project: None,
                    labels: BTreeMap::new(),
                    specification,
                };
                apply::<Node>(store, &reference, change).await?;
            }
            NodeAction::ReportReady { observed_generation, status } => {
                apply::<Node>(store, &reference, Change::UpdateStatus { observed_generation, status })
                    .await?;
            }
            NodeAction::Remove => remove::<Node>(store, &reference).await?,
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use amafu_domain::Metadata;

    use super::*;

    fn identity() -> NodeIdentity {
        NodeIdentity {
            name: NodeName::new("node-a").unwrap(),
            id: ObjectId::from_uuid(uuid::Uuid::new_v4()),
            roles: BTreeSet::from([NodeRole::Core, NodeRole::Compute]),
            capacity: NodeCapacity { cpu_count: 8, memory_mebibytes: 16_384 },
            software_version: String::from("0.1.0"),
            heartbeat_interval: Duration::from_secs(10),
        }
    }

    fn at(seconds: u64) -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn registered(identity: &NodeIdentity, status: NodeStatus) -> Object<Node> {
        Object {
            metadata: Metadata {
                name: ObjectName::new(identity.name.as_str()).unwrap(),
                project: None,
                id: identity.id,
                version: 1,
                generation: 1,
                observed_generation: 1,
                labels: BTreeMap::new(),
                is_deleting: false,
            },
            specification: NodeSpecification::default(),
            status,
        }
    }

    fn ready(identity: &NodeIdentity, heartbeat: OffsetDateTime) -> NodeStatus {
        NodeStatus {
            phase: NodePhase::Ready,
            capacity: identity.capacity,
            software_version: identity.software_version.clone(),
            last_heartbeat: Some(heartbeat),
        }
    }

    #[test]
    fn an_unknown_node_registers_itself() {
        let identity = identity();
        let actions = plan_node(&NodeObservation { identity: identity.clone(), node: None, now: at(0) });
        assert_eq!(
            actions,
            vec![NodeAction::Register(NodeSpecification { roles: identity.roles, is_cordoned: false })]
        );
    }

    #[test]
    fn a_registered_node_reports_ready() {
        let identity = identity();
        let node = registered(&identity, NodeStatus::default());
        let actions =
            plan_node(&NodeObservation { identity: identity.clone(), node: Some(node), now: at(5) });
        assert_eq!(
            actions,
            vec![NodeAction::ReportReady { observed_generation: 1, status: ready(&identity, at(5)) }]
        );
    }

    #[test]
    fn a_recent_heartbeat_needs_nothing() {
        let identity = identity();
        let node = registered(&identity, ready(&identity, at(0)));
        assert_eq!(plan_node(&NodeObservation { identity, node: Some(node), now: at(9) }), Vec::new());
    }

    #[test]
    fn an_old_heartbeat_is_renewed() {
        let identity = identity();
        let node = registered(&identity, ready(&identity, at(0)));
        let actions =
            plan_node(&NodeObservation { identity: identity.clone(), node: Some(node), now: at(10) });
        assert_eq!(
            actions,
            vec![NodeAction::ReportReady { observed_generation: 1, status: ready(&identity, at(10)) }]
        );
    }

    #[test]
    fn a_deleting_node_is_removed() {
        let identity = identity();
        let mut node = registered(&identity, ready(&identity, at(0)));
        node.metadata.is_deleting = true;
        assert_eq!(
            plan_node(&NodeObservation { identity, node: Some(node), now: at(1) }),
            vec![NodeAction::Remove]
        );
    }
}
