use std::collections::{BTreeSet, HashMap};

use amafu_domain::kinds::node::CpuBaseline;
use amafu_domain::{NodeName, ObjectId, ZoneName};

use crate::workload::PendingWorkload;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateNode {
    pub name: NodeName,
    pub zone: Option<ZoneName>,
    pub is_cordoned: bool,
    pub cpu_baseline: CpuBaseline,
    pub allocatable_cpu_count: u32,
    pub allocatable_memory_mebibytes: u64,
    pub allocated_cpu_count: u32,
    pub allocated_memory_mebibytes: u64,
    pub cached_images: BTreeSet<ObjectId>,
    pub anti_affinity_groups: BTreeSet<String>,
}

impl CandidateNode {
    #[must_use]
    pub fn new(name: NodeName, allocatable_cpu_count: u32, allocatable_memory_mebibytes: u64) -> Self {
        Self {
            name,
            zone: None,
            is_cordoned: false,
            cpu_baseline: CpuBaseline::default(),
            allocatable_cpu_count,
            allocatable_memory_mebibytes,
            allocated_cpu_count: 0,
            allocated_memory_mebibytes: 0,
            cached_images: BTreeSet::new(),
            anti_affinity_groups: BTreeSet::new(),
        }
    }

    #[must_use]
    pub fn free_cpu_count(&self) -> u32 {
        self.allocatable_cpu_count.saturating_sub(self.allocated_cpu_count)
    }

    #[must_use]
    pub fn free_memory_mebibytes(&self) -> u64 {
        self.allocatable_memory_mebibytes.saturating_sub(self.allocated_memory_mebibytes)
    }

    fn zone_key(&self) -> &str {
        self.zone.as_ref().map_or("", ZoneName::as_str)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClusterCapacity {
    nodes: Vec<CandidateNode>,
    zone_loads: HashMap<String, HashMap<String, u32>>,
}

impl ClusterCapacity {
    #[must_use]
    pub fn new(mut nodes: Vec<CandidateNode>) -> Self {
        nodes.sort_by(|left, right| left.name.cmp(&right.name));
        Self { nodes, zone_loads: HashMap::new() }
    }

    pub fn nodes(&self) -> impl Iterator<Item = &CandidateNode> {
        self.nodes.iter()
    }

    #[must_use]
    pub fn node(&self, name: &NodeName) -> Option<&CandidateNode> {
        self.nodes.iter().find(|node| &node.name == name)
    }

    pub fn count_in_zone(&mut self, zone: Option<&ZoneName>, spread_group: &str) {
        let zone = zone.map_or("", ZoneName::as_str);
        *self.zone_loads.entry(zone.to_owned()).or_default().entry(spread_group.to_owned()).or_default() += 1;
    }

    #[must_use]
    pub fn zone_load(&self, node: &CandidateNode, spread_group: &str) -> u32 {
        self.zone_loads.get(node.zone_key()).and_then(|groups| groups.get(spread_group)).copied().unwrap_or(0)
    }

    pub fn reserve(&mut self, name: &NodeName, workload: &PendingWorkload) {
        let Some(node) = self.nodes.iter_mut().find(|node| &node.name == name) else {
            return;
        };
        node.allocated_cpu_count = node.allocated_cpu_count.saturating_add(workload.cpu_count);
        node.allocated_memory_mebibytes =
            node.allocated_memory_mebibytes.saturating_add(workload.memory_mebibytes);
        if let Some(group) = &workload.anti_affinity_group {
            node.anti_affinity_groups.insert(group.clone());
        }
        let zone = node.zone.clone();
        self.count_in_zone(zone.as_ref(), &workload.spread_group);
    }
}
