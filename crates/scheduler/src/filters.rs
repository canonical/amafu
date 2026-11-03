use crate::cluster::CandidateNode;
use crate::place::PlacementFilter;
use crate::workload::PendingWorkload;

pub struct Cordoned;

impl PlacementFilter for Cordoned {
    fn allows(&self, _workload: &PendingWorkload, node: &CandidateNode) -> bool {
        !node.is_cordoned
    }

    fn rejection(&self) -> &'static str {
        "cordoned"
    }
}

pub struct Capacity;

impl PlacementFilter for Capacity {
    fn allows(&self, workload: &PendingWorkload, node: &CandidateNode) -> bool {
        workload.cpu_count <= node.free_cpu_count()
            && workload.memory_mebibytes <= node.free_memory_mebibytes()
    }

    fn rejection(&self) -> &'static str {
        "without enough free CPU or memory"
    }
}

pub struct CpuCompatibility;

impl PlacementFilter for CpuCompatibility {
    fn allows(&self, workload: &PendingWorkload, node: &CandidateNode) -> bool {
        node.cpu_baseline >= workload.minimum_cpu_baseline
    }

    fn rejection(&self) -> &'static str {
        "below the required CPU baseline"
    }
}

pub struct StorageReachability;

impl PlacementFilter for StorageReachability {
    fn allows(&self, workload: &PendingWorkload, node: &CandidateNode) -> bool {
        workload.required_node.as_ref().is_none_or(|required| *required == node.name)
            && workload.required_images.iter().all(|image| node.cached_images.contains(image))
    }

    fn rejection(&self) -> &'static str {
        "unable to reach its volumes or images"
    }
}

pub struct AntiAffinity;

impl PlacementFilter for AntiAffinity {
    fn allows(&self, workload: &PendingWorkload, node: &CandidateNode) -> bool {
        workload.anti_affinity_group.as_ref().is_none_or(|group| !node.anti_affinity_groups.contains(group))
    }

    fn rejection(&self) -> &'static str {
        "already running a member of its anti-affinity group"
    }
}
