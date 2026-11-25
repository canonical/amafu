use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use std::net::Ipv4Addr;

use amafu_domain::features::Feature;
use amafu_domain::kinds::cluster_version::{ClusterVersion, cluster_version_name};
use amafu_domain::kinds::node::{
    BootReport, CpuBaseline, Node, NodeCapacity, NodePhase, NodeRole, NodeSpecification, NodeStatus,
    StoreHealth,
};
use amafu_domain::{Change, NodeName, Object, ObjectId, ObjectName, ObjectReference, ZoneName};
use amafu_interfaces::objects::{apply, get, remove};
use amafu_interfaces::{
    ActionError, BootSlots, Clock, ClusterAddressClaim, ConsensusMembership, Consistency, Store,
};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeIdentity {
    pub name: NodeName,
    pub id: ObjectId,
    pub roles: std::collections::BTreeSet<NodeRole>,
    pub capacity: NodeCapacity,
    pub cpu_baseline: CpuBaseline,
    pub zone: Option<ZoneName>,
    pub underlay_address: Option<Ipv4Addr>,
    pub software_version: String,
    pub heartbeat_interval: Duration,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LocalReport {
    pub boot: Option<BootReport>,
    pub store: Option<StoreHealth>,
    pub holds_cluster_address: bool,
}

impl NodeIdentity {
    #[must_use]
    pub fn status(&self, now: OffsetDateTime, report: LocalReport) -> NodeStatus {
        NodeStatus {
            store: report.store,
            holds_cluster_address: report.holds_cluster_address,
            phase: NodePhase::Ready,
            capacity: self.capacity,
            cpu_baseline: self.cpu_baseline,
            underlay_address: self.underlay_address,
            software_version: self.software_version.clone(),
            last_heartbeat: Some(now),
            boot: report.boot,
        }
    }

    #[must_use]
    pub fn registered_roles(&self, is_cluster_address_enabled: bool) -> std::collections::BTreeSet<NodeRole> {
        let mut roles = self.roles.clone();
        if is_cluster_address_enabled && roles.contains(&NodeRole::Core) {
            roles.insert(NodeRole::Dashboard);
        }
        roles
    }
}

pub struct NodeController {
    identity: NodeIdentity,
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    boot_slots: Option<Arc<dyn BootSlots>>,
    consensus: Option<Arc<dyn ConsensusMembership>>,
    cluster_address: Option<Arc<dyn ClusterAddressClaim>>,
}

impl NodeController {
    #[must_use]
    pub fn new(identity: NodeIdentity, store: Arc<dyn Store>, clock: Arc<dyn Clock>) -> Self {
        Self { identity, store, clock, boot_slots: None, consensus: None, cluster_address: None }
    }

    #[must_use]
    pub fn with_boot_slots(mut self, boot_slots: Arc<dyn BootSlots>) -> Self {
        self.boot_slots = Some(boot_slots);
        self
    }

    #[must_use]
    pub fn with_consensus(mut self, consensus: Arc<dyn ConsensusMembership>) -> Self {
        self.consensus = Some(consensus);
        self
    }

    #[must_use]
    pub fn with_cluster_address(mut self, cluster_address: Arc<dyn ClusterAddressClaim>) -> Self {
        self.cluster_address = Some(cluster_address);
        self
    }

    fn reference(&self) -> ObjectReference {
        node_reference(&self.identity.name)
    }

    fn local_report(&self) -> LocalReport {
        LocalReport {
            boot: self.boot_slots.as_ref().map(|boot_slots| boot_slots.report()),
            store: self.consensus.as_ref().and_then(|consensus| consensus.health()),
            holds_cluster_address: self.cluster_address.as_ref().is_some_and(|claim| claim.holds()),
        }
    }

    async fn is_cluster_address_enabled(&self) -> Result<bool, ActionError> {
        let reference = ObjectReference::of::<ClusterVersion>(None, cluster_version_name());
        let version =
            get::<ClusterVersion>(self.store.as_ref(), &reference, Consistency::Linearizable).await?;
        Ok(version.is_some_and(|version| Feature::ClusterAddress.is_enabled(&version.specification)))
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
    pub report: LocalReport,
    pub is_cluster_address_enabled: bool,
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
            roles: identity.registered_roles(observation.is_cluster_address_enabled),
            is_cordoned: false,
            zone: identity.zone.clone(),
            image: None,
        })];
    };
    if node.metadata.is_deleting {
        return vec![NodeAction::Remove];
    }
    let current = identity.status(observation.now, observation.report.clone());
    let reported = NodeStatus { last_heartbeat: node.status.last_heartbeat, ..current.clone() };
    let is_current = node.is_status_current()
        && node.status == reported
        && !is_heartbeat_due(&node.status, observation.now, identity.heartbeat_interval);
    if is_current {
        return Vec::new();
    }
    vec![NodeAction::ReportReady { observed_generation: node.metadata.generation, status: current }]
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
        let is_cluster_address_enabled = node.is_none() && self.is_cluster_address_enabled().await?;
        Ok(NodeObservation {
            identity: self.identity.clone(),
            node,
            report: self.local_report(),
            is_cluster_address_enabled,
            now: self.clock.now(),
        })
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
            cpu_baseline: CpuBaseline::X86_64Version3,
            zone: None,
            underlay_address: Some(Ipv4Addr::new(10, 0, 0, 1)),
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
        identity.status(heartbeat, LocalReport::default())
    }

    fn observed(identity: &NodeIdentity, node: Option<Object<Node>>, now: OffsetDateTime) -> NodeObservation {
        NodeObservation {
            identity: identity.clone(),
            node,
            report: LocalReport::default(),
            is_cluster_address_enabled: false,
            now,
        }
    }

    #[test]
    fn an_unknown_node_registers_itself() {
        let identity = identity();
        let actions = plan_node(&observed(&identity, None, at(0)));
        assert_eq!(
            actions,
            vec![NodeAction::Register(NodeSpecification {
                roles: identity.roles,
                is_cordoned: false,
                zone: None,
                image: None
            })]
        );
    }

    #[test]
    fn a_core_member_registers_as_a_dashboard_node_once_the_cluster_address_is_enabled() {
        let identity = identity();
        let observation =
            NodeObservation { is_cluster_address_enabled: true, ..observed(&identity, None, at(0)) };
        let actions = plan_node(&observation);
        let Some(NodeAction::Register(specification)) = actions.first() else {
            panic!("the node must register, not {actions:?}");
        };
        assert!(specification.serves_dashboard());
        let compute_only = NodeIdentity { roles: BTreeSet::from([NodeRole::Compute]), ..identity };
        assert_eq!(compute_only.registered_roles(true), BTreeSet::from([NodeRole::Compute]));
    }

    #[test]
    fn a_registered_node_reports_ready() {
        let identity = identity();
        let node = registered(&identity, NodeStatus::default());
        let actions = plan_node(&observed(&identity, Some(node), at(5)));
        assert_eq!(
            actions,
            vec![NodeAction::ReportReady { observed_generation: 1, status: ready(&identity, at(5)) }]
        );
    }

    #[test]
    fn a_recent_heartbeat_needs_nothing() {
        let identity = identity();
        let node = registered(&identity, ready(&identity, at(0)));
        assert_eq!(plan_node(&observed(&identity, Some(node), at(9))), Vec::new());
    }

    #[test]
    fn a_change_in_store_health_is_reported_before_the_next_heartbeat() {
        let identity = identity();
        let node = registered(&identity, ready(&identity, at(0)));
        let unsettled = StoreHealth { is_stable: false, elections_started: 3, ..StoreHealth::default() };
        let report = LocalReport { store: Some(unsettled), ..LocalReport::default() };
        let actions = plan_node(&NodeObservation { report, ..observed(&identity, Some(node), at(2)) });
        let reported = NodeStatus { store: Some(unsettled), ..ready(&identity, at(2)) };
        assert_eq!(actions, vec![NodeAction::ReportReady { observed_generation: 1, status: reported }]);
    }

    #[test]
    fn an_old_heartbeat_is_renewed() {
        let identity = identity();
        let node = registered(&identity, ready(&identity, at(0)));
        let actions = plan_node(&observed(&identity, Some(node), at(10)));
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
        assert_eq!(plan_node(&observed(&identity, Some(node), at(1))), vec![NodeAction::Remove]);
    }
}
