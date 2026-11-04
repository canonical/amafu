use serde::{Deserialize, Serialize};

use crate::access::ClusterPolicy;
use crate::features::FeatureLevel;
use crate::name::ObjectName;
use crate::node_image::NodeImage;
use crate::resource::{Resource, Scope};
use crate::validation::{Validate, ValidationError};

pub const CLUSTER_VERSION_NAME: &str = "cluster";

#[must_use]
pub fn cluster_version_name() -> ObjectName {
    ObjectName::new(CLUSTER_VERSION_NAME).unwrap_or_else(|_| unreachable!())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterVersion;

impl Resource for ClusterVersion {
    const KIND: &'static str = "ClusterVersion";
    const PLURAL_NAME: &'static str = "cluster-versions";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Cluster;
    const CLUSTER_POLICY: ClusterPolicy = ClusterPolicy::AdminOnly;
    type Specification = ClusterVersionSpecification;
    type Status = ClusterVersionStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterVersionSpecification {
    pub image: NodeImage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_image: Option<NodeImage>,
    pub finalized_feature_level: FeatureLevel,
}

impl ClusterVersionSpecification {
    #[must_use]
    pub fn installed(image: NodeImage) -> Self {
        let finalized_feature_level = image.feature_level;
        Self { image, previous_image: None, finalized_feature_level }
    }

    #[must_use]
    pub fn moved_to(&self, image: NodeImage) -> Self {
        Self { previous_image: Some(self.image.clone()), image, ..self.clone() }
    }

    #[must_use]
    pub fn finalized(&self) -> Self {
        Self {
            previous_image: None,
            finalized_feature_level: self.image.feature_level,
            image: self.image.clone(),
        }
    }

    #[must_use]
    pub fn admits(&self, image: &NodeImage) -> bool {
        image.feature_level >= self.finalized_feature_level
    }
}

impl Validate for ClusterVersionSpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        self.image.check("specification.image", &mut error);
        if let Some(previous) = &self.previous_image {
            previous.check("specification.previous_image", &mut error);
            error.check(
                self.admits(previous),
                "specification.previous_image",
                "must support the finalized feature level",
            );
        }
        error.check(
            self.admits(&self.image),
            "specification.finalized_feature_level",
            "must not be above the feature level of the image",
        );
        error.into_result()
    }

    fn validate_update(&self, previous: &Self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(
            self.finalized_feature_level >= previous.finalized_feature_level,
            "specification.finalized_feature_level",
            "cannot be lowered",
        );
        error.into_result()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ClusterVersionStatus {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::Feature;

    fn image(version: &str, feature_level: u32) -> NodeImage {
        NodeImage {
            version: version.to_owned(),
            feature_level: FeatureLevel(feature_level),
            digest: format!("{feature_level:0>64}").parse().unwrap(),
            size_bytes: 1024,
            url: format!("https://images.example/{version}.tar"),
            signature: "0".repeat(128),
        }
    }

    #[test]
    fn finalizing_enables_the_features_of_the_image_and_forgets_the_way_back() {
        let installed = ClusterVersionSpecification::installed(image("0.1.0", 1));
        let upgraded = installed.moved_to(image("0.2.0", 2));
        assert_eq!(upgraded.validate(), Ok(()));
        assert!(!Feature::ParallelMigrationStreams.is_enabled(&upgraded));
        assert_eq!(upgraded.previous_image, Some(image("0.1.0", 1)));
        let finalized = upgraded.finalized();
        assert!(Feature::ParallelMigrationStreams.is_enabled(&finalized));
        assert!(!Feature::DedicatedSubscriptionRelays.is_enabled(&finalized));
        assert_eq!(finalized.previous_image, None);
        assert_eq!(finalized.validate_update(&upgraded), Ok(()));
        assert!(upgraded.validate_update(&finalized).is_err());
    }

    #[test]
    fn an_image_below_the_finalized_level_is_refused() {
        let finalized = ClusterVersionSpecification::installed(image("0.2.0", 2));
        assert!(!finalized.admits(&image("0.1.0", 1)));
        assert!(finalized.moved_to(image("0.1.0", 1)).validate().is_err());
    }
}
