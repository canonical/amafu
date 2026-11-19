use serde::{Deserialize, Serialize};

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
