use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::kinds::operation::StepSpecification;
use crate::name::NodeName;
use crate::node_image::NodeImage;
use crate::validation::ValidationError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallPlan {
    pub image: NodeImage,
    pub nodes: Vec<NodeName>,
}

impl InstallPlan {
    #[must_use]
    pub fn steps(&self) -> Vec<StepSpecification> {
        let deployments =
            self.nodes.iter().map(|node| StepSpecification::DeployMachine { node: node.clone() });
        let joins = self.nodes.iter().map(|node| StepSpecification::AwaitHealthy { node: node.clone() });
        std::iter::once(StepSpecification::VerifyImage)
            .chain(deployments)
            .chain(joins)
            .chain(std::iter::once(StepSpecification::SetClusterImage))
            .collect()
    }

    pub fn check(&self, error: &mut ValidationError) {
        self.image.check("specification.plan.image", error);
        let mut seen = BTreeSet::new();
        error.check(!self.nodes.is_empty(), "specification.plan.nodes", "must name at least one node");
        error.check(
            self.nodes.iter().all(|node| seen.insert(node)),
            "specification.plan.nodes",
            "must name each node once",
        );
    }
}
