use serde::{Deserialize, Serialize};

use crate::name::ObjectName;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnvironmentPhase {
    #[default]
    Creating,
    Ready,
    Changing,
    Degraded,
    Deleting,
    Failed,
}

pub trait EnvironmentChange {
    fn environment(&self) -> &ObjectName;

    fn creates_environment(&self) -> bool;
}

pub trait EnvironmentStatus {
    fn phase(&self) -> EnvironmentPhase;

    #[must_use]
    fn failed(&self, reason: &str) -> Self;
}
