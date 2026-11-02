use serde::{Deserialize, Serialize};

use crate::name::{NodeName, ObjectName};
use crate::resource::{Resource, Scope};
use crate::validation::{Validate, ValidationError};

pub const MAXIMUM_CPU_COUNT: u16 = 256;
pub const MINIMUM_MEMORY_MEBIBYTES: u64 = 64;
pub const MAXIMUM_MEMORY_MEBIBYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vm;

impl Resource for Vm {
    const KIND: &'static str = "Vm";
    const PLURAL_NAME: &'static str = "vms";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Project;
    type Specification = VmSpecification;
    type Status = VmStatus;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PowerState {
    #[default]
    Running,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmSpecification {
    pub cpu_count: u16,
    pub memory_mebibytes: u64,
    pub image: ObjectName,
    #[serde(default)]
    pub power_state: PowerState,
}

impl VmSpecification {
    #[must_use]
    pub fn small() -> Self {
        Self {
            cpu_count: 1,
            memory_mebibytes: 256,
            image: ObjectName::new("test-guest").unwrap_or_else(|_| unreachable!()),
            power_state: PowerState::Running,
        }
    }

    #[must_use]
    pub fn should_run(&self) -> bool {
        self.power_state == PowerState::Running
    }
}

impl Validate for VmSpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(
            (1..=MAXIMUM_CPU_COUNT).contains(&self.cpu_count),
            "specification.cpu_count",
            "must be between 1 and 256",
        );
        error.check(
            (MINIMUM_MEMORY_MEBIBYTES..=MAXIMUM_MEMORY_MEBIBYTES).contains(&self.memory_mebibytes),
            "specification.memory_mebibytes",
            "must be between 64 MiB and 4 TiB",
        );
        error.into_result()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VmPhase {
    #[default]
    Pending,
    Scheduled,
    Starting,
    Running,
    Stopping,
    Stopped,
    Failed,
}

impl VmPhase {
    #[must_use]
    pub fn can_become(self, next: Self) -> bool {
        use VmPhase::{Failed, Pending, Running, Scheduled, Starting, Stopped, Stopping};
        self == next
            || matches!(
                (self, next),
                (_, Failed | Pending)
                    | (Pending, Scheduled)
                    | (Scheduled | Stopped | Failed, Starting)
                    | (Scheduled | Starting | Running | Stopping, Stopped)
                    | (Starting, Running | Stopping)
                    | (Running, Stopping)
            )
    }

    #[must_use]
    pub fn is_settled(self) -> bool {
        matches!(self, Self::Running | Self::Stopped | Self::Failed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct VmStatus {
    pub phase: VmPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<NodeName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_small_specification_is_valid() {
        assert_eq!(VmSpecification::small().validate(), Ok(()));
    }

    #[test]
    fn limits_are_enforced() {
        let mut specification = VmSpecification::small();
        specification.cpu_count = MAXIMUM_CPU_COUNT + 1;
        specification.memory_mebibytes = MINIMUM_MEMORY_MEBIBYTES - 1;
        let error = specification.validate().unwrap_err();
        assert_eq!(error.violations.len(), 2);
    }

    #[test]
    fn unknown_fields_are_refused() {
        let body = r#"{"cpu_count":1,"memory_mebibytes":256,"image":"test-guest","gpu":true}"#;
        assert!(serde_json::from_str::<VmSpecification>(body).is_err());
    }

    #[test]
    fn phases_follow_the_lifecycle() {
        assert!(VmPhase::Pending.can_become(VmPhase::Scheduled));
        assert!(VmPhase::Scheduled.can_become(VmPhase::Starting));
        assert!(VmPhase::Starting.can_become(VmPhase::Running));
        assert!(VmPhase::Running.can_become(VmPhase::Stopping));
        assert!(VmPhase::Stopping.can_become(VmPhase::Stopped));
        assert!(VmPhase::Stopped.can_become(VmPhase::Starting));
        assert!(VmPhase::Running.can_become(VmPhase::Failed));
        assert!(!VmPhase::Pending.can_become(VmPhase::Running));
        assert!(!VmPhase::Stopped.can_become(VmPhase::Running));
        assert!(!VmPhase::Stopping.can_become(VmPhase::Running));
    }
}
