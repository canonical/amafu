use std::fmt;

use serde::{Deserialize, Serialize};

use crate::kinds::cluster_version::ClusterVersionSpecification;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FeatureLevel(pub u32);

impl fmt::Display for FeatureLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

pub const SUPPORTED_FEATURE_LEVEL: FeatureLevel = FeatureLevel(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Feature {
    ParallelMigrationStreams,
    DedicatedSubscriptionRelays,
}

impl Feature {
    #[must_use]
    pub const fn introduced_in(self) -> FeatureLevel {
        match self {
            Self::ParallelMigrationStreams => FeatureLevel(2),
            Self::DedicatedSubscriptionRelays => FeatureLevel(3),
        }
    }

    #[must_use]
    pub fn is_enabled(self, cluster: &ClusterVersionSpecification) -> bool {
        cluster.finalized_feature_level >= self.introduced_in()
    }
}
