use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::kinds::operation::StepSpecification;
use crate::name::{NodeName, ZoneName};
use crate::node_image::NodeImage;
use crate::validation::ValidationError;

pub const DEFAULT_HEALTH_WINDOW_SECONDS: u64 = 120;
pub const DEFAULT_SOAK_SECONDS: u64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WavePolicy {
    pub canary_count: usize,
    pub health_window_seconds: u64,
    pub soak_seconds: u64,
}

impl Default for WavePolicy {
    fn default() -> Self {
        Self {
            canary_count: 1,
            health_window_seconds: DEFAULT_HEALTH_WINDOW_SECONDS,
            soak_seconds: DEFAULT_SOAK_SECONDS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryNode {
    pub name: NodeName,
    pub zone: Option<ZoneName>,
    pub is_core: bool,
    pub runs_target: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpgradeWave {
    pub nodes: Vec<NodeName>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpgradePlan {
    pub image: NodeImage,
    pub waves: Vec<UpgradeWave>,
    pub health_window_seconds: u64,
    pub soak_seconds: u64,
    #[serde(default)]
    pub is_rollback: bool,
}

#[must_use]
pub fn waves(inventory: &[InventoryNode], policy: &WavePolicy) -> Vec<UpgradeWave> {
    let mut pending: Vec<&InventoryNode> = inventory.iter().filter(|node| !node.runs_target).collect();
    pending.sort_by(|left, right| left.name.cmp(&right.name));
    let (core, others): (Vec<&InventoryNode>, Vec<&InventoryNode>) =
        pending.into_iter().partition(|node| node.is_core);
    let canary_count = policy.canary_count.min(others.len());
    let (canaries, rest) = others.split_at(canary_count);
    let mut waves = Vec::new();
    if !canaries.is_empty() {
        waves.push(UpgradeWave { nodes: canaries.iter().map(|node| node.name.clone()).collect() });
    }
    let mut by_zone: BTreeMap<Option<&ZoneName>, Vec<NodeName>> = BTreeMap::new();
    for node in rest {
        by_zone.entry(node.zone.as_ref()).or_default().push(node.name.clone());
    }
    waves.extend(by_zone.into_values().map(|nodes| UpgradeWave { nodes }));
    waves.extend(core.into_iter().map(|node| UpgradeWave { nodes: vec![node.name.clone()] }));
    waves
}

impl UpgradePlan {
    #[must_use]
    pub fn upgrade(inventory: &[InventoryNode], image: NodeImage, policy: &WavePolicy) -> Self {
        Self::new(image, waves(inventory, policy), policy, false)
    }

    #[must_use]
    pub fn rollback(inventory: &[InventoryNode], previous_image: NodeImage, policy: &WavePolicy) -> Self {
        let mut reversed = waves(inventory, policy);
        reversed.reverse();
        for wave in &mut reversed {
            wave.nodes.reverse();
        }
        Self::new(previous_image, reversed, policy, true)
    }

    fn new(image: NodeImage, waves: Vec<UpgradeWave>, policy: &WavePolicy, is_rollback: bool) -> Self {
        Self {
            image,
            waves,
            health_window_seconds: policy.health_window_seconds,
            soak_seconds: policy.soak_seconds,
            is_rollback,
        }
    }

    pub fn nodes(&self) -> impl Iterator<Item = &NodeName> {
        self.waves.iter().flat_map(|wave| wave.nodes.iter())
    }

    #[must_use]
    pub fn steps(&self) -> Vec<StepSpecification> {
        let mut steps = vec![StepSpecification::VerifyImage, StepSpecification::SetClusterImage];
        for wave in &self.waves {
            for node in &wave.nodes {
                steps.push(StepSpecification::Evacuate { node: node.clone() });
                steps.push(StepSpecification::WriteInactiveSlot { node: node.clone() });
                steps.push(StepSpecification::RebootIntoSlot { node: node.clone() });
                steps.push(StepSpecification::AwaitHealthy { node: node.clone() });
                steps.push(StepSpecification::Uncordon { node: node.clone() });
            }
            steps.push(StepSpecification::Soak);
        }
        steps
    }

    pub fn check(&self, error: &mut ValidationError) {
        self.image.check("specification.plan.image", error);
        let mut seen = BTreeSet::new();
        error.check(
            self.waves.iter().all(|wave| !wave.nodes.is_empty()),
            "specification.plan.waves",
            "must not hold an empty wave",
        );
        error.check(
            self.nodes().all(|node| seen.insert(node)),
            "specification.plan.waves",
            "must name each node once",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::FeatureLevel;

    fn node(name: &str, zone: Option<&str>, is_core: bool) -> InventoryNode {
        InventoryNode {
            name: NodeName::new(name).unwrap(),
            zone: zone.map(|zone| ZoneName::new(zone).unwrap()),
            is_core,
            runs_target: false,
        }
    }

    fn image() -> NodeImage {
        NodeImage {
            version: String::from("0.2.0"),
            feature_level: FeatureLevel(1),
            digest: "b".repeat(64).parse().unwrap(),
            size_bytes: 1024,
            url: String::from("https://images.example/0.2.0.tar"),
            signature: "0".repeat(128),
        }
    }

    fn names(wave: &UpgradeWave) -> Vec<&str> {
        wave.nodes.iter().map(NodeName::as_str).collect()
    }

    #[test]
    fn a_canary_goes_first_then_each_zone_and_the_core_voters_one_by_one_last() {
        let inventory = [
            node("node-0", Some("rack-1"), true),
            node("node-1", Some("rack-1"), false),
            node("node-2", Some("rack-2"), false),
            node("node-3", Some("rack-1"), false),
            node("node-4", Some("rack-2"), true),
            node("node-5", Some("rack-2"), false),
        ];
        let planned = waves(&inventory, &WavePolicy::default());
        let planned: Vec<Vec<&str>> = planned.iter().map(names).collect();
        assert_eq!(
            planned,
            vec![vec!["node-1"], vec!["node-3"], vec!["node-2", "node-5"], vec!["node-0"], vec!["node-4"]]
        );
    }

    #[test]
    fn nodes_already_on_the_target_are_left_out() {
        let mut done = node("node-1", None, false);
        done.runs_target = true;
        let planned = waves(&[done, node("node-2", None, false)], &WavePolicy::default());
        assert_eq!(planned, vec![UpgradeWave { nodes: vec![NodeName::new("node-2").unwrap()] }]);
    }

    #[test]
    fn a_rollback_runs_the_same_waves_backwards() {
        let inventory =
            [node("node-0", None, true), node("node-1", None, false), node("node-2", None, false)];
        let policy = WavePolicy::default();
        let forward = UpgradePlan::upgrade(&inventory, image(), &policy);
        let backward = UpgradePlan::rollback(&inventory, image(), &policy);
        let forward_order: Vec<&NodeName> = forward.nodes().collect();
        let mut backward_order: Vec<&NodeName> = backward.nodes().collect();
        backward_order.reverse();
        assert_eq!(forward_order, backward_order);
        assert!(backward.is_rollback);
    }

    #[test]
    fn every_node_is_evacuated_written_rebooted_awaited_and_uncordoned_in_order() {
        let plan = UpgradePlan::upgrade(&[node("node-1", None, false)], image(), &WavePolicy::default());
        let names: Vec<&str> = plan.steps().iter().map(StepSpecification::name).collect();
        assert_eq!(
            names,
            vec![
                "verify-image",
                "set-cluster-image",
                "evacuate",
                "write-inactive-slot",
                "reboot-into-slot",
                "await-healthy",
                "uncordon",
                "soak"
            ]
        );
    }
}
