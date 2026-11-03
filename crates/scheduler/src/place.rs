use std::cmp::Reverse;

use amafu_domain::{NodeName, ObjectId};

use crate::cluster::{CandidateNode, ClusterCapacity};
use crate::filters::{AntiAffinity, Capacity, Cordoned, CpuCompatibility, StorageReachability};
use crate::scores::LeastAllocated;
use crate::workload::PendingWorkload;

pub trait PlacementFilter: Send + Sync {
    fn allows(&self, workload: &PendingWorkload, node: &CandidateNode) -> bool;

    fn rejection(&self) -> &'static str;
}

pub trait PlacementScore: Send + Sync {
    fn score(&self, workload: &PendingWorkload, node: &CandidateNode) -> i64;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacementDecision {
    Place(ObjectId, NodeName),
    Unschedulable(ObjectId, String),
}

pub struct Scheduler {
    pub filters: Vec<Box<dyn PlacementFilter>>,
    pub scores: Vec<Box<dyn PlacementScore>>,
}

impl Scheduler {
    #[must_use]
    pub fn standard() -> Self {
        Self {
            filters: vec![
                Box::new(Cordoned),
                Box::new(Capacity),
                Box::new(CpuCompatibility),
                Box::new(StorageReachability),
                Box::new(AntiAffinity),
            ],
            scores: vec![Box::new(LeastAllocated)],
        }
    }

    fn allows(&self, workload: &PendingWorkload, node: &CandidateNode) -> bool {
        self.filters.iter().all(|filter| filter.allows(workload, node))
    }

    fn total_score(&self, workload: &PendingWorkload, node: &CandidateNode) -> i64 {
        self.scores.iter().map(|score| score.score(workload, node)).fold(0, i64::saturating_add)
    }

    #[must_use]
    pub fn explain_rejection(&self, workload: &PendingWorkload, cluster: &ClusterCapacity) -> String {
        let mut counts: Vec<(&'static str, usize)> = Vec::new();
        for node in cluster.nodes() {
            let Some(filter) = self.filters.iter().find(|filter| !filter.allows(workload, node)) else {
                continue;
            };
            match counts.iter_mut().find(|(reason, _)| *reason == filter.rejection()) {
                Some((_, count)) => *count += 1,
                None => counts.push((filter.rejection(), 1)),
            }
        }
        if counts.is_empty() {
            return String::from("no node accepts workloads");
        }
        let reasons: Vec<String> = counts
            .iter()
            .map(|(reason, count)| {
                format!("{count} {} {reason}", if *count == 1 { "node is" } else { "nodes are" })
            })
            .collect();
        format!("no node fits: {}", reasons.join(", "))
    }

    fn best_node(&self, workload: &PendingWorkload, cluster: &ClusterCapacity) -> Option<NodeName> {
        let mut best: Option<(i64, Reverse<u32>, &CandidateNode)> = None;
        for node in cluster.nodes().filter(|node| self.allows(workload, node)) {
            let score = self.total_score(workload, node);
            if best.as_ref().is_some_and(|(best_score, _, _)| score < *best_score) {
                continue;
            }
            let zone_load = Reverse(cluster.zone_load(node, &workload.spread_group));
            let is_better = match &best {
                None => true,
                Some((best_score, best_zone_load, _)) => score > *best_score || zone_load > *best_zone_load,
            };
            if is_better {
                best = Some((score, zone_load, node));
            }
        }
        best.map(|(_, _, node)| node.name.clone())
    }
}

#[must_use]
pub fn place(
    scheduler: &Scheduler,
    mut cluster: ClusterCapacity,
    mut pending: Vec<PendingWorkload>,
) -> Vec<PlacementDecision> {
    pending.sort_by_key(|workload| Reverse(workload.priority));
    pending
        .into_iter()
        .map(|workload| match scheduler.best_node(&workload, &cluster) {
            Some(node) => {
                cluster.reserve(&node, &workload);
                PlacementDecision::Place(workload.id, node)
            }
            None => PlacementDecision::Unschedulable(
                workload.id,
                scheduler.explain_rejection(&workload, &cluster),
            ),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use amafu_domain::ZoneName;
    use amafu_domain::kinds::node::CpuBaseline;
    use proptest::prelude::*;

    use super::*;

    fn node_name(index: usize) -> NodeName {
        NodeName::new(format!("node-{index}")).unwrap()
    }

    fn id(value: u128) -> ObjectId {
        ObjectId::from_uuid(uuid::Uuid::from_u128(value))
    }

    fn node(index: usize, cpu_count: u32, memory_mebibytes: u64) -> CandidateNode {
        CandidateNode::new(node_name(index), cpu_count, memory_mebibytes)
    }

    fn placed_on(decisions: &[PlacementDecision]) -> Vec<Option<NodeName>> {
        decisions
            .iter()
            .map(|decision| match decision {
                PlacementDecision::Place(_, node) => Some(node.clone()),
                PlacementDecision::Unschedulable(_, _) => None,
            })
            .collect()
    }

    #[test]
    fn workloads_go_to_the_least_allocated_node() {
        let mut busy = node(0, 4, 4096);
        busy.allocated_cpu_count = 2;
        busy.allocated_memory_mebibytes = 2048;
        let cluster = ClusterCapacity::new(vec![busy, node(1, 4, 4096)]);
        let decisions = place(&Scheduler::standard(), cluster, vec![PendingWorkload::new(id(1), 1, 512)]);
        assert_eq!(placed_on(&decisions), vec![Some(node_name(1))]);
    }

    #[test]
    fn ties_are_broken_by_name() {
        let cluster = ClusterCapacity::new(vec![node(2, 4, 4096), node(1, 4, 4096)]);
        let decisions = place(&Scheduler::standard(), cluster, vec![PendingWorkload::new(id(1), 1, 512)]);
        assert_eq!(placed_on(&decisions), vec![Some(node_name(1))]);
    }

    #[test]
    fn each_placement_reserves_capacity_before_the_next() {
        let cluster = ClusterCapacity::new(vec![node(0, 2, 1024), node(1, 2, 1024)]);
        let pending = (1..=3).map(|value| PendingWorkload::new(id(value), 1, 1024)).collect();
        let decisions = place(&Scheduler::standard(), cluster, pending);
        assert_eq!(placed_on(&decisions), vec![Some(node_name(0)), Some(node_name(1)), None]);
        let PlacementDecision::Unschedulable(_, reason) = &decisions[2] else { unreachable!() };
        assert_eq!(reason, "no node fits: 2 nodes are without enough free CPU or memory");
    }

    #[test]
    fn higher_priorities_are_placed_first() {
        let cluster = ClusterCapacity::new(vec![node(0, 1, 1024)]);
        let mut important = PendingWorkload::new(id(2), 1, 1024);
        important.priority = 10;
        let decisions =
            place(&Scheduler::standard(), cluster, vec![PendingWorkload::new(id(1), 1, 1024), important]);
        assert_eq!(decisions[0], PlacementDecision::Place(id(2), node_name(0)));
        assert!(matches!(decisions[1], PlacementDecision::Unschedulable(workload, _) if workload == id(1)));
    }

    #[test]
    fn filters_reject_cordoned_incompatible_and_unreachable_nodes() {
        let mut cordoned = node(0, 8, 8192);
        cordoned.is_cordoned = true;
        let mut old = node(1, 8, 8192);
        old.cpu_baseline = CpuBaseline::X86_64;
        let mut modern = node(2, 8, 8192);
        modern.cpu_baseline = CpuBaseline::X86_64Version3;
        let mut workload = PendingWorkload::new(id(1), 1, 512);
        workload.minimum_cpu_baseline = CpuBaseline::X86_64Version2;
        let cluster = ClusterCapacity::new(vec![cordoned, old, modern]);
        assert_eq!(
            placed_on(&place(&Scheduler::standard(), cluster.clone(), vec![workload.clone()])),
            vec![Some(node_name(2))]
        );
        workload.required_node = Some(node_name(1));
        let decisions = place(&Scheduler::standard(), cluster, vec![workload]);
        let PlacementDecision::Unschedulable(_, reason) = &decisions[0] else { unreachable!() };
        assert_eq!(
            reason,
            "no node fits: 1 node is cordoned, 1 node is below the required CPU baseline, \
             1 node is unable to reach its volumes or images"
        );
    }

    #[test]
    fn workloads_needing_an_image_go_where_it_is_cached() {
        let mut cached = node(1, 4, 4096);
        cached.cached_images.insert(id(99));
        let cluster = ClusterCapacity::new(vec![node(0, 4, 4096), cached]);
        let mut workload = PendingWorkload::new(id(1), 1, 512);
        workload.required_images.insert(id(99));
        assert_eq!(
            placed_on(&place(&Scheduler::standard(), cluster, vec![workload])),
            vec![Some(node_name(1))]
        );
    }

    #[test]
    fn members_of_an_anti_affinity_group_land_on_different_nodes() {
        let cluster = ClusterCapacity::new(vec![node(0, 64, 65_536), node(1, 1, 1024)]);
        let pending: Vec<PendingWorkload> = (1..=3)
            .map(|value| {
                let mut workload = PendingWorkload::new(id(value), 1, 256);
                workload.anti_affinity_group = Some(String::from("project-a/web"));
                workload
            })
            .collect();
        let decisions = place(&Scheduler::standard(), cluster, pending);
        assert_eq!(placed_on(&decisions), vec![Some(node_name(0)), Some(node_name(1)), None]);
    }

    #[test]
    fn equal_nodes_are_chosen_to_spread_a_group_across_zones() {
        let zoned = |index: usize, zone: &str| {
            let mut candidate = node(index, 8, 8192);
            candidate.zone = Some(ZoneName::new(zone).unwrap());
            candidate
        };
        let mut cluster =
            ClusterCapacity::new(vec![zoned(0, "zone-a"), zoned(1, "zone-a"), zoned(2, "zone-b")]);
        cluster.count_in_zone(Some(&ZoneName::new("zone-a").unwrap()), "project-a");
        let mut workload = PendingWorkload::new(id(1), 1, 512);
        workload.spread_group = String::from("project-a");
        assert_eq!(
            placed_on(&place(&Scheduler::standard(), cluster, vec![workload])),
            vec![Some(node_name(2))]
        );
    }

    #[test]
    fn an_empty_cluster_explains_itself() {
        let decisions = place(
            &Scheduler::standard(),
            ClusterCapacity::default(),
            vec![PendingWorkload::new(id(1), 1, 512)],
        );
        assert_eq!(
            decisions,
            vec![PlacementDecision::Unschedulable(id(1), String::from("no node accepts workloads"))]
        );
    }

    fn arbitrary_cluster() -> impl Strategy<Value = Vec<CandidateNode>> {
        prop::collection::vec((1_u32..16, 256_u64..16_384, any::<bool>()), 0..8).prop_map(|nodes| {
            nodes
                .into_iter()
                .enumerate()
                .map(|(index, (cpu_count, memory, is_cordoned))| {
                    let mut candidate = node(index, cpu_count, memory);
                    candidate.is_cordoned = is_cordoned;
                    candidate
                })
                .collect()
        })
    }

    fn arbitrary_workloads() -> impl Strategy<Value = Vec<PendingWorkload>> {
        prop::collection::vec((1_u32..8, 64_u64..4096, -5_i32..5, prop::option::of(0_u8..3)), 0..24).prop_map(
            |workloads| {
                workloads
                    .into_iter()
                    .enumerate()
                    .map(|(index, (cpu_count, memory, priority, group))| {
                        let mut workload = PendingWorkload::new(id(index as u128), cpu_count, memory);
                        workload.priority = priority;
                        workload.anti_affinity_group = group.map(|group| format!("group-{group}"));
                        workload
                    })
                    .collect()
            },
        )
    }

    proptest! {
        #[test]
        fn placements_never_overcommit_or_break_rules(nodes in arbitrary_cluster(), workloads in arbitrary_workloads()) {
            let cluster = ClusterCapacity::new(nodes.clone());
            let decisions = place(&Scheduler::standard(), cluster.clone(), workloads.clone());
            prop_assert_eq!(decisions.clone(), place(&Scheduler::standard(), cluster, workloads.clone()));
            let mut used: BTreeMap<NodeName, (u32, u64)> = BTreeMap::new();
            let mut groups: BTreeMap<(NodeName, String), usize> = BTreeMap::new();
            for decision in &decisions {
                let PlacementDecision::Place(workload_id, node_name) = decision else { continue };
                let workload = workloads.iter().find(|workload| workload.id == *workload_id).unwrap();
                let candidate = nodes.iter().find(|candidate| candidate.name == *node_name).unwrap();
                prop_assert!(!candidate.is_cordoned);
                let entry = used.entry(node_name.clone()).or_default();
                entry.0 += workload.cpu_count;
                entry.1 += workload.memory_mebibytes;
                prop_assert!(entry.0 <= candidate.allocatable_cpu_count);
                prop_assert!(entry.1 <= candidate.allocatable_memory_mebibytes);
                if let Some(group) = &workload.anti_affinity_group {
                    let count = groups.entry((node_name.clone(), group.clone())).or_default();
                    *count += 1;
                    prop_assert_eq!(*count, 1);
                }
            }
        }
    }
}
