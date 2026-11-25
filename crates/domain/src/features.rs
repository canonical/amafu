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

pub const SUPPORTED_FEATURE_LEVEL: FeatureLevel = FeatureLevel(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Feature {
    ClusterAddress,
    ParallelMigrationStreams,
    DedicatedSubscriptionRelays,
}

impl Feature {
    #[must_use]
    pub const fn introduced_in(self) -> FeatureLevel {
        match self {
            Self::ClusterAddress => FeatureLevel(2),
            Self::ParallelMigrationStreams => FeatureLevel(3),
            Self::DedicatedSubscriptionRelays => FeatureLevel(4),
        }
    }

    #[must_use]
    pub fn is_enabled(self, cluster: &ClusterVersionSpecification) -> bool {
        cluster.finalized_feature_level >= self.introduced_in()
    }
}
