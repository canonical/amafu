use crate::cluster::CandidateNode;
use crate::place::PlacementScore;
use crate::workload::PendingWorkload;

const SCALE: u128 = 1000;

pub struct LeastAllocated;

fn free_share_after(allocatable: u64, free: u64, requested: u64) -> u128 {
    if allocatable == 0 {
        return 0;
    }
    u128::from(free.saturating_sub(requested)) * SCALE / u128::from(allocatable)
}

impl PlacementScore for LeastAllocated {
    fn score(&self, workload: &PendingWorkload, node: &CandidateNode) -> i64 {
        let cpu = free_share_after(
            u64::from(node.allocatable_cpu_count),
            u64::from(node.free_cpu_count()),
            u64::from(workload.cpu_count),
        );
        let memory = free_share_after(
            node.allocatable_memory_mebibytes,
            node.free_memory_mebibytes(),
            workload.memory_mebibytes,
        );
        i64::try_from(u128::midpoint(cpu, memory)).unwrap_or(i64::MAX)
    }
}
