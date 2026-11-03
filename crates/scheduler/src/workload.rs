use std::collections::BTreeSet;

use amafu_domain::kinds::node::CpuBaseline;
use amafu_domain::{NodeName, ObjectId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingWorkload {
    pub id: ObjectId,
    pub priority: i32,
    pub cpu_count: u32,
    pub memory_mebibytes: u64,
    pub minimum_cpu_baseline: CpuBaseline,
    pub required_node: Option<NodeName>,
    pub required_images: BTreeSet<ObjectId>,
    pub anti_affinity_group: Option<String>,
    pub spread_group: String,
}

impl PendingWorkload {
    #[must_use]
    pub fn new(id: ObjectId, cpu_count: u32, memory_mebibytes: u64) -> Self {
        Self {
            id,
            priority: 0,
            cpu_count,
            memory_mebibytes,
            minimum_cpu_baseline: CpuBaseline::default(),
            required_node: None,
            required_images: BTreeSet::new(),
            anti_affinity_group: None,
            spread_group: String::new(),
        }
    }
}
