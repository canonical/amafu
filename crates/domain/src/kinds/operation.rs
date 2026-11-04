use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::name::{NodeName, ObjectName, ProjectName};
use crate::resource::{Resource, Scope};
use crate::validation::{Validate, ValidationError};

pub const MAXIMUM_STEPS: usize = 1_000;
const MAXIMUM_ERROR_LENGTH: usize = 1_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operation;

impl Resource for Operation {
    const KIND: &'static str = "Operation";
    const PLURAL_NAME: &'static str = "operations";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Cluster;
    type Specification = OperationSpecification;
    type Status = OperationStatus;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OperationKind {
    Migration,
    Recovery,
    Evacuation,
}

impl fmt::Display for OperationKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Migration => "migration",
            Self::Recovery => "recovery",
            Self::Evacuation => "evacuation",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmReference {
    pub project: ProjectName,
    pub name: ObjectName,
}

impl fmt::Display for VmReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}", self.project, self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum OperationTarget {
    Node { node: NodeName },
    Vm { vm: VmReference },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "step", deny_unknown_fields)]
pub enum StepSpecification {
    MarkNodeLost {
        node: NodeName,
    },
    RevokeNodeOnStorage {
        node: NodeName,
    },
    WaitOutWatchdog {
        node: NodeName,
    },
    ReleaseAssignments {
        node: NodeName,
    },
    PrepareDestination {
        vm: VmReference,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        destination: Option<NodeName>,
    },
    CopyMemory {
        vm: VmReference,
    },
    SwitchOver {
        vm: VmReference,
    },
    AnnounceAddresses {
        vm: VmReference,
    },
    CleanUpSource {
        vm: VmReference,
    },
    Cordon {
        node: NodeName,
    },
    MigrateVm {
        vm: VmReference,
    },
}

impl StepSpecification {
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::MarkNodeLost { .. } => "mark-node-lost",
            Self::RevokeNodeOnStorage { .. } => "revoke-node-on-storage",
            Self::WaitOutWatchdog { .. } => "wait-out-watchdog",
            Self::ReleaseAssignments { .. } => "release-assignments",
            Self::PrepareDestination { .. } => "prepare-destination",
            Self::CopyMemory { .. } => "copy-memory",
            Self::SwitchOver { .. } => "switch-over",
            Self::AnnounceAddresses { .. } => "announce-addresses",
            Self::CleanUpSource { .. } => "clean-up-source",
            Self::Cordon { .. } => "cordon",
            Self::MigrateVm { .. } => "migrate-vm",
        }
    }

    #[must_use]
    pub fn can_be_undone(&self) -> bool {
        !matches!(
            self,
            Self::MarkNodeLost { .. }
                | Self::ReleaseAssignments { .. }
                | Self::SwitchOver { .. }
                | Self::AnnounceAddresses { .. }
                | Self::CleanUpSource { .. }
                | Self::MigrateVm { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationSpecification {
    pub kind: OperationKind,
    pub target: OperationTarget,
    pub steps: Vec<StepSpecification>,
    #[serde(default)]
    pub is_paused: bool,
    #[serde(default)]
    pub is_cancelled: bool,
}

impl OperationSpecification {
    #[must_use]
    pub fn recovery(node: NodeName) -> Self {
        let steps = recovery_steps(&node);
        Self::new(OperationKind::Recovery, OperationTarget::Node { node }, steps)
    }

    #[must_use]
    pub fn migration(vm: VmReference, destination: Option<NodeName>) -> Self {
        let steps = migration_steps(&vm, destination);
        Self::new(OperationKind::Migration, OperationTarget::Vm { vm }, steps)
    }

    #[must_use]
    pub fn evacuation(node: NodeName, vms: Vec<VmReference>) -> Self {
        let steps = std::iter::once(StepSpecification::Cordon { node: node.clone() })
            .chain(vms.into_iter().map(|vm| StepSpecification::MigrateVm { vm }))
            .collect();
        Self::new(OperationKind::Evacuation, OperationTarget::Node { node }, steps)
    }

    fn new(kind: OperationKind, target: OperationTarget, steps: Vec<StepSpecification>) -> Self {
        Self { kind, target, steps, is_paused: false, is_cancelled: false }
    }

    fn has_expected_steps(&self) -> bool {
        match (&self.kind, &self.target) {
            (OperationKind::Recovery, OperationTarget::Node { node }) => self.steps == recovery_steps(node),
            (OperationKind::Migration, OperationTarget::Vm { vm }) => {
                let destination = match self.steps.first() {
                    Some(StepSpecification::PrepareDestination { destination, .. }) => destination.clone(),
                    _ => None,
                };
                self.steps == migration_steps(vm, destination)
            }
            (OperationKind::Evacuation, OperationTarget::Node { node }) => {
                is_evacuation_of(node, &self.steps)
            }
            _ => false,
        }
    }
}

fn recovery_steps(node: &NodeName) -> Vec<StepSpecification> {
    vec![
        StepSpecification::MarkNodeLost { node: node.clone() },
        StepSpecification::RevokeNodeOnStorage { node: node.clone() },
        StepSpecification::WaitOutWatchdog { node: node.clone() },
        StepSpecification::ReleaseAssignments { node: node.clone() },
    ]
}

fn migration_steps(vm: &VmReference, destination: Option<NodeName>) -> Vec<StepSpecification> {
    vec![
        StepSpecification::PrepareDestination { vm: vm.clone(), destination },
        StepSpecification::CopyMemory { vm: vm.clone() },
        StepSpecification::SwitchOver { vm: vm.clone() },
        StepSpecification::AnnounceAddresses { vm: vm.clone() },
        StepSpecification::CleanUpSource { vm: vm.clone() },
    ]
}

fn is_evacuation_of(node: &NodeName, steps: &[StepSpecification]) -> bool {
    let Some((StepSpecification::Cordon { node: cordoned }, migrations)) = steps.split_first() else {
        return false;
    };
    let mut migrated = BTreeSet::new();
    cordoned == node
        && migrations.iter().all(|step| match step {
            StepSpecification::MigrateVm { vm } => migrated.insert(vm),
            _ => false,
        })
}

impl Validate for OperationSpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(self.steps.len() <= MAXIMUM_STEPS, "specification.steps", "must have at most 1000 steps");
        error.check(
            self.has_expected_steps(),
            "specification.steps",
            "must be the steps of the operation kind",
        );
        error.into_result()
    }

    fn validate_update(&self, previous: &Self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(self.kind == previous.kind, "specification.kind", "cannot change");
        error.check(self.target == previous.target, "specification.target", "cannot change");
        error.check(self.steps == previous.steps, "specification.steps", "cannot change");
        error.check(
            self.is_cancelled || !previous.is_cancelled,
            "specification.is_cancelled",
            "cannot be withdrawn",
        );
        error.into_result()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepProgress {
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
}

impl StepProgress {
    #[must_use]
    pub fn started(at: OffsetDateTime) -> Self {
        Self { attempts: 0, last_error: None, started_at: at }
    }

    #[must_use]
    pub fn failed_attempt(&self, error: &str) -> Self {
        let mut last_error = error.to_owned();
        last_error.truncate(MAXIMUM_ERROR_LENGTH);
        Self { attempts: self.attempts.saturating_add(1), last_error: Some(last_error), ..self.clone() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "outcome")]
pub enum OperationOutcome {
    Succeeded,
    Failed { reason: String },
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rollback {
    pub reason: RollbackReason,
    pub remaining_steps: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "reason")]
pub enum RollbackReason {
    Failed { error: String },
    Cancelled,
}

impl RollbackReason {
    #[must_use]
    pub fn outcome(&self) -> OperationOutcome {
        match self {
            Self::Failed { error } => OperationOutcome::Failed { reason: error.clone() },
            Self::Cancelled => OperationOutcome::Cancelled,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OperationStatus {
    #[serde(default)]
    pub completed_steps: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_step: Option<StepProgress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback: Option<Rollback>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<OperationOutcome>,
}

impl OperationStatus {
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.outcome.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str) -> NodeName {
        NodeName::new(name).unwrap()
    }

    fn vm(name: &str) -> VmReference {
        VmReference { project: ProjectName::new("project-a").unwrap(), name: ObjectName::new(name).unwrap() }
    }

    #[test]
    fn each_kind_is_built_with_its_own_steps() {
        assert_eq!(OperationSpecification::recovery(node("node-1")).validate(), Ok(()));
        assert_eq!(OperationSpecification::migration(vm("web-1"), Some(node("node-2"))).validate(), Ok(()));
        assert_eq!(OperationSpecification::migration(vm("web-1"), None).validate(), Ok(()));
        assert_eq!(
            OperationSpecification::evacuation(node("node-1"), vec![vm("web-1"), vm("web-2")]).validate(),
            Ok(())
        );
    }

    #[test]
    fn steps_that_do_not_match_the_kind_are_refused() {
        let mut skipped_fencing = OperationSpecification::recovery(node("node-1"));
        skipped_fencing.steps.remove(1);
        assert!(skipped_fencing.validate().is_err());
        let mut other_node = OperationSpecification::recovery(node("node-1"));
        other_node.steps[0] = StepSpecification::MarkNodeLost { node: node("node-2") };
        assert!(other_node.validate().is_err());
        let twice = OperationSpecification::evacuation(node("node-1"), vec![vm("web-1"), vm("web-1")]);
        assert!(twice.validate().is_err());
        let mut wrong_target = OperationSpecification::migration(vm("web-1"), None);
        wrong_target.target = OperationTarget::Vm { vm: vm("web-2") };
        assert!(wrong_target.validate().is_err());
    }

    #[test]
    fn only_pausing_and_cancelling_may_change() {
        let created = OperationSpecification::migration(vm("web-1"), None);
        let paused = OperationSpecification { is_paused: true, ..created.clone() };
        assert_eq!(paused.validate_update(&created), Ok(()));
        let cancelled = OperationSpecification { is_cancelled: true, ..created.clone() };
        assert_eq!(cancelled.validate_update(&created), Ok(()));
        assert!(created.validate_update(&cancelled).is_err());
        let redirected = OperationSpecification::migration(vm("web-1"), Some(node("node-3")));
        assert!(redirected.validate_update(&created).is_err());
    }

    #[test]
    fn steps_past_the_switch_over_cannot_be_undone() {
        let undoable: Vec<bool> = OperationSpecification::migration(vm("web-1"), None)
            .steps
            .iter()
            .map(StepSpecification::can_be_undone)
            .collect();
        assert_eq!(undoable, vec![true, true, false, false, false]);
    }
}
