use std::sync::Arc;

use amafu_domain::Change;
use amafu_domain::kinds::cluster_version::{ClusterVersion, ClusterVersionSpecification};
use amafu_domain::kinds::node::{BootReport, Node};
use amafu_domain::kinds::operation::{Operation, OperationKind};
use amafu_interfaces::objects::{apply, list};
use amafu_interfaces::{ActionError, Consistency, Store};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};

use crate::operations::upgrade::{cluster_version, cluster_version_reference};

#[derive(Debug, Clone, PartialEq)]
pub struct ClusterImageObservation {
    pub recorded: Option<(u64, ClusterVersionSpecification)>,
    pub boots: Vec<Option<BootReport>>,
    pub is_upgrading: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClusterImageAction {
    Revert { expected_generation: u64, specification: ClusterVersionSpecification },
}

#[must_use]
pub fn plan_cluster_image(observation: &ClusterImageObservation) -> Vec<ClusterImageAction> {
    let Some((generation, recorded)) = &observation.recorded else {
        return Vec::new();
    };
    let Some(reverted) = recorded.reverted() else {
        return Vec::new();
    };
    let boots = &observation.boots;
    let has_landed = boots.iter().flatten().any(|boot| boot.runs(&recorded.image));
    let every_node_runs_the_previous = !boots.is_empty()
        && boots.iter().all(|boot| boot.as_ref().is_some_and(|boot| boot.runs(&reverted.image)));
    if observation.is_upgrading || has_landed || !every_node_runs_the_previous {
        return Vec::new();
    }
    vec![ClusterImageAction::Revert { expected_generation: *generation, specification: reverted }]
}

pub struct ClusterImageController {
    store: Arc<dyn Store>,
}

impl ClusterImageController {
    #[must_use]
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }
}

impl Controller for ClusterImageController {
    type Key = ();
    type Observation = ClusterImageObservation;
    type Action = ClusterImageAction;

    const NAME: &'static str = "cluster-image";
    const WATCHES: &'static [WatchedKind<()>] = &[
        WatchedKind { kind: "ClusterVersion", mapping: KeyMapping::EveryKey },
        WatchedKind { kind: "Node", mapping: KeyMapping::EveryKey },
        WatchedKind { kind: "Operation", mapping: KeyMapping::EveryKey },
    ];

    async fn discover_keys(&self) -> Result<Vec<()>, ActionError> {
        Ok(vec![()])
    }

    async fn observe(&self, _key: &()) -> Result<ClusterImageObservation, ActionError> {
        let store = self.store.as_ref();
        let recorded =
            cluster_version(store).await?.map(|cluster| (cluster.metadata.generation, cluster.specification));
        let boots = list::<Node>(store, None, Consistency::Linearizable)
            .await?
            .into_iter()
            .map(|node| node.status.boot)
            .collect();
        let is_upgrading =
            list::<Operation>(store, None, Consistency::Linearizable).await?.iter().any(|operation| {
                operation.specification.kind == OperationKind::Upgrade && !operation.status.is_finished()
            });
        Ok(ClusterImageObservation { recorded, boots, is_upgrading })
    }

    fn plan(&self, observation: &ClusterImageObservation) -> Vec<ClusterImageAction> {
        plan_cluster_image(observation)
    }

    async fn apply(&self, _key: &(), action: ClusterImageAction) -> Result<(), ActionError> {
        let ClusterImageAction::Revert { expected_generation, specification } = action;
        tracing::warn!(image = %specification.image.identity(), "no node runs the recorded cluster image; going back");
        apply::<ClusterVersion>(
            self.store.as_ref(),
            &cluster_version_reference(),
            Change::UpdateSpecification { expected_generation: Some(expected_generation), specification },
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use amafu_domain::features::FeatureLevel;
    use amafu_domain::kinds::node::BootSlot;
    use amafu_domain::node_image::NodeImage;

    use super::*;

    fn image(version: &str, digest: char) -> NodeImage {
        NodeImage {
            version: version.to_owned(),
            feature_level: FeatureLevel(1),
            digest: digest.to_string().repeat(64).parse().unwrap(),
            size_bytes: 1,
            url: String::from("https://images.example/node-image.tar"),
            signature: "0".repeat(128),
        }
    }

    fn running(image: &NodeImage) -> BootReport {
        BootReport {
            booted_slot: BootSlot::First,
            default_slot: BootSlot::First,
            booted_image: Some(image.identity()),
            inactive_image: None,
            fallen_back_from: None,
            refused: None,
        }
    }

    fn observation(boots: Vec<Option<BootReport>>, is_upgrading: bool) -> ClusterImageObservation {
        let recorded =
            ClusterVersionSpecification::installed(image("26.11.1", 'a')).moved_to(image("26.11.2", 'b'));
        ClusterImageObservation { recorded: Some((4, recorded)), boots, is_upgrading }
    }

    #[test]
    fn an_upgrade_that_landed_nowhere_goes_back_to_the_previous_image() {
        let old = image("26.11.1", 'a');
        let actions = plan_cluster_image(&observation(vec![Some(running(&old)), Some(running(&old))], false));
        assert!(matches!(
            actions.as_slice(),
            [ClusterImageAction::Revert { expected_generation: 4, specification }]
                if specification.image == old && specification.previous_image.is_none()
        ));
    }

    #[test]
    fn a_running_upgrade_or_a_node_on_the_new_image_keeps_the_record() {
        let (old, new) = (image("26.11.1", 'a'), image("26.11.2", 'b'));
        assert_eq!(plan_cluster_image(&observation(vec![Some(running(&old))], true)), Vec::new());
        assert_eq!(
            plan_cluster_image(&observation(vec![Some(running(&old)), Some(running(&new))], false)),
            Vec::new()
        );
        assert_eq!(plan_cluster_image(&observation(vec![Some(running(&old)), None], false)), Vec::new());
    }
}
