use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use amafu_domain::kinds::node::{Node, NodePhase, NodeStatus};
use amafu_domain::kinds::node_lease::{NodeLease, NodeLeaseSpecification};
use amafu_domain::kinds::operation::{Operation, OperationKind, OperationSpecification, OperationTarget};
use amafu_domain::{Change, ChangeError, NodeName, Object, ObjectId, ObjectName, ObjectReference};
use amafu_interfaces::objects::{apply, get, list, update_status};
use amafu_interfaces::{ActionError, Clock, Consistency, Identifiers, ObjectStoreError, Store};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};
use time::OffsetDateTime;

const SHORTEST_REVISIT: Duration = Duration::from_millis(250);

pub struct NodeHealthController {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    identifiers: Arc<dyn Identifiers>,
}

impl NodeHealthController {
    #[must_use]
    pub fn new(store: Arc<dyn Store>, clock: Arc<dyn Clock>, identifiers: Arc<dyn Identifiers>) -> Self {
        Self { store, clock, identifiers }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodeHealthObservation {
    pub node_name: Option<NodeName>,
    pub lease: Option<NodeLeaseSpecification>,
    pub node: Option<Object<Node>>,
    pub is_recovering: bool,
    pub now: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeHealthAction {
    MarkSuspect,
    StartRecovery { node: NodeName, epoch: u64 },
}

#[must_use]
pub fn plan_node_health(observation: &NodeHealthObservation) -> Vec<NodeHealthAction> {
    let (Some(node_name), Some(lease)) = (&observation.node_name, &observation.lease) else {
        return Vec::new();
    };
    if lease.is_lost || !lease.has_expired(observation.now) {
        return Vec::new();
    }
    let mut actions = Vec::new();
    if observation.node.as_ref().is_some_and(|node| node.status.phase == NodePhase::Ready) {
        actions.push(NodeHealthAction::MarkSuspect);
    }
    if !observation.is_recovering {
        actions.push(NodeHealthAction::StartRecovery { node: node_name.clone(), epoch: lease.epoch });
    }
    actions
}

#[must_use]
pub fn recovery_name(node: &NodeName, epoch: u64) -> String {
    format!("recovery-{node}-{epoch}")
}

fn is_recovery_of(operation: &Object<Operation>, node: &NodeName) -> bool {
    !operation.status.is_finished()
        && matches!(&operation.specification.target, OperationTarget::Node { node: target } if target == node)
        && operation.specification.kind == OperationKind::Recovery
}

impl Controller for NodeHealthController {
    type Key = ObjectReference;
    type Observation = NodeHealthObservation;
    type Action = NodeHealthAction;

    const NAME: &'static str = "node-health";
    const WATCHES: &'static [WatchedKind<ObjectReference>] = &[
        WatchedKind {
            kind: "NodeLease",
            mapping: KeyMapping::ObjectKey(|reference| Some(reference.clone())),
        },
        WatchedKind { kind: "Operation", mapping: KeyMapping::EveryKey },
    ];

    async fn discover_keys(&self) -> Result<Vec<ObjectReference>, ActionError> {
        let leases = list::<NodeLease>(self.store.as_ref(), None, Consistency::Linearizable).await?;
        Ok(leases.iter().map(Object::reference).collect())
    }

    async fn observe(&self, key: &ObjectReference) -> Result<NodeHealthObservation, ActionError> {
        let store = self.store.as_ref();
        let now = self.clock.now();
        let node_name = NodeName::new(key.name.as_str()).ok();
        let lease =
            get::<NodeLease>(store, key, Consistency::Linearizable).await?.map(|lease| lease.specification);
        let node_reference = ObjectReference::of::<Node>(None, key.name.clone());
        let node = get::<Node>(store, &node_reference, Consistency::Linearizable).await?;
        let is_recovering = match &node_name {
            Some(name) => list::<Operation>(store, None, Consistency::Linearizable)
                .await?
                .iter()
                .any(|operation| is_recovery_of(operation, name)),
            None => false,
        };
        Ok(NodeHealthObservation { node_name, lease, node, is_recovering, now })
    }

    fn plan(&self, observation: &NodeHealthObservation) -> Vec<NodeHealthAction> {
        plan_node_health(observation)
    }

    async fn apply(&self, key: &ObjectReference, action: NodeHealthAction) -> Result<(), ActionError> {
        match action {
            NodeHealthAction::MarkSuspect => {
                let reference = ObjectReference::of::<Node>(None, key.name.clone());
                update_status::<Node>(self.store.as_ref(), &reference, |node| {
                    (node.status.phase == NodePhase::Ready)
                        .then(|| NodeStatus { phase: NodePhase::Suspect, ..node.status.clone() })
                })
                .await?;
                Ok(())
            }
            NodeHealthAction::StartRecovery { node, epoch } => self.start_recovery(&node, epoch).await,
        }
    }

    fn revisit_after(&self, observation: &NodeHealthObservation) -> Option<Duration> {
        let lease = observation.lease.as_ref().filter(|lease| !lease.is_lost)?;
        let remaining = Duration::try_from(lease.expires_at() - observation.now).unwrap_or_default();
        Some(remaining.max(SHORTEST_REVISIT))
    }
}

impl NodeHealthController {
    async fn start_recovery(&self, node: &NodeName, epoch: u64) -> Result<(), ActionError> {
        let name = ObjectName::new(recovery_name(node, epoch)).map_err(ActionError::invalid)?;
        let reference = ObjectReference::of::<Operation>(None, name.clone());
        let created = apply::<Operation>(
            self.store.as_ref(),
            &reference,
            Change::Create {
                id: ObjectId::from_uuid(self.identifiers.generate()),
                name,
                project: None,
                labels: BTreeMap::new(),
                specification: OperationSpecification::recovery(node.clone()),
            },
        )
        .await;
        match created {
            Ok(_) => {
                tracing::warn!(%node, epoch, "a node's lease expired; recovering its VMs");
                Ok(())
            }
            Err(ObjectStoreError::Change(ChangeError::AlreadyExists)) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use amafu_domain::kinds::node::NodeSpecification;
    use amafu_domain::kinds::node_lease::LeaseTerms;

    use super::*;
    use crate::testing::metadata;

    fn at(seconds: i64) -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(seconds)
    }

    fn observation(phase: NodePhase, now: i64, is_recovering: bool) -> NodeHealthObservation {
        let terms = LeaseTerms::new(Duration::from_secs(8), Duration::from_secs(8));
        NodeHealthObservation {
            node_name: Some(NodeName::new("node-1").unwrap()),
            lease: Some(NodeLeaseSpecification::acquired(None, terms, at(0)).unwrap()),
            node: Some(Object {
                metadata: metadata("node-1", None),
                specification: NodeSpecification::default(),
                status: NodeStatus { phase, ..NodeStatus::default() },
            }),
            is_recovering,
            now: at(now),
        }
    }

    #[test]
    fn a_node_whose_lease_expired_is_suspected_and_recovered_once() {
        assert_eq!(plan_node_health(&observation(NodePhase::Ready, 7, false)), Vec::new());
        let node = NodeName::new("node-1").unwrap();
        assert_eq!(
            plan_node_health(&observation(NodePhase::Ready, 8, false)),
            vec![NodeHealthAction::MarkSuspect, NodeHealthAction::StartRecovery { node, epoch: 1 }]
        );
        assert_eq!(plan_node_health(&observation(NodePhase::Suspect, 9, true)), Vec::new());
    }

    #[test]
    fn a_lost_lease_needs_no_second_recovery() {
        let mut lost = observation(NodePhase::Lost, 30, false);
        lost.lease = lost.lease.map(|lease| lease.marked_lost(at(9)).unwrap());
        assert_eq!(plan_node_health(&lost), Vec::new());
    }
}
