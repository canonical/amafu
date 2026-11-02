use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::access::ClusterPolicy;
use crate::resource::{Resource, Scope};
use crate::validation::{Validate, ValidationError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node;

impl Resource for Node {
    const KIND: &'static str = "Node";
    const PLURAL_NAME: &'static str = "nodes";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Cluster;
    const CLUSTER_POLICY: ClusterPolicy = ClusterPolicy::ManagedByNodes;
    type Specification = NodeSpecification;
    type Status = NodeStatus;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeRole {
    Core,
    Compute,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeSpecification {
    pub roles: BTreeSet<NodeRole>,
    #[serde(default)]
    pub is_cordoned: bool,
}

impl Default for NodeSpecification {
    fn default() -> Self {
        Self { roles: BTreeSet::from([NodeRole::Core, NodeRole::Compute]), is_cordoned: false }
    }
}

impl NodeSpecification {
    #[must_use]
    pub fn accepts_workloads(&self) -> bool {
        self.roles.contains(&NodeRole::Compute) && !self.is_cordoned
    }
}

impl Validate for NodeSpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(!self.roles.is_empty(), "specification.roles", "must name at least one role");
        error.into_result()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodePhase {
    #[default]
    Unknown,
    Ready,
    NotReady,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NodeCapacity {
    pub cpu_count: u32,
    pub memory_mebibytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NodeStatus {
    pub phase: NodePhase,
    pub capacity: NodeCapacity,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub software_version: String,
    #[serde(default, with = "time::serde::rfc3339::option", skip_serializing_if = "Option::is_none")]
    pub last_heartbeat: Option<OffsetDateTime>,
}

impl NodeStatus {
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.phase == NodePhase::Ready
    }
}
