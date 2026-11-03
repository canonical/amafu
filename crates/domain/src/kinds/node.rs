use std::collections::BTreeSet;
use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::access::ClusterPolicy;
use crate::name::ZoneName;
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone: Option<ZoneName>,
}

impl Default for NodeSpecification {
    fn default() -> Self {
        Self { roles: BTreeSet::from([NodeRole::Core, NodeRole::Compute]), is_cordoned: false, zone: None }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
pub enum CpuBaseline {
    #[default]
    #[serde(rename = "x86-64")]
    X86_64,
    #[serde(rename = "x86-64-v2")]
    X86_64Version2,
    #[serde(rename = "x86-64-v3")]
    X86_64Version3,
    #[serde(rename = "x86-64-v4")]
    X86_64Version4,
}

impl CpuBaseline {
    const VERSION_2_FLAGS: &'static [&'static str] =
        &["cx16", "lahf_lm", "popcnt", "sse4_1", "sse4_2", "ssse3"];
    const VERSION_3_FLAGS: &'static [&'static str] =
        &["avx", "avx2", "bmi1", "bmi2", "f16c", "fma", "abm", "movbe", "xsave"];
    const VERSION_4_FLAGS: &'static [&'static str] =
        &["avx512f", "avx512bw", "avx512cd", "avx512dq", "avx512vl"];

    #[must_use]
    pub fn from_flags<'flags>(flags: impl IntoIterator<Item = &'flags str>) -> Self {
        let present: BTreeSet<&str> = flags.into_iter().collect();
        let has_all = |required: &[&str]| required.iter().all(|flag| present.contains(flag));
        if !has_all(Self::VERSION_2_FLAGS) {
            Self::X86_64
        } else if !has_all(Self::VERSION_3_FLAGS) {
            Self::X86_64Version2
        } else if !has_all(Self::VERSION_4_FLAGS) {
            Self::X86_64Version3
        } else {
            Self::X86_64Version4
        }
    }
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
    #[serde(default)]
    pub cpu_baseline: CpuBaseline,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underlay_address: Option<Ipv4Addr>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cpu_baseline_is_the_highest_level_whose_flags_are_all_present() {
        assert_eq!(CpuBaseline::from_flags(["sse2"]), CpuBaseline::X86_64);
        let version_2 = CpuBaseline::VERSION_2_FLAGS.to_vec();
        assert_eq!(CpuBaseline::from_flags(version_2.clone()), CpuBaseline::X86_64Version2);
        let version_3: Vec<&str> = version_2.iter().chain(CpuBaseline::VERSION_3_FLAGS).copied().collect();
        assert_eq!(CpuBaseline::from_flags(version_3.clone()), CpuBaseline::X86_64Version3);
        let version_4: Vec<&str> = version_3.iter().chain(CpuBaseline::VERSION_4_FLAGS).copied().collect();
        assert_eq!(CpuBaseline::from_flags(version_4), CpuBaseline::X86_64Version4);
        assert!(CpuBaseline::X86_64Version2 < CpuBaseline::X86_64Version3);
    }

    #[test]
    fn baselines_are_written_with_their_standard_names() {
        assert_eq!(serde_json::to_string(&CpuBaseline::X86_64Version3).unwrap(), "\"x86-64-v3\"");
    }
}
