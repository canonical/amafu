use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::install::InstallPlan;
use crate::name::{NodeName, ObjectName, ProjectName};
use crate::node_image::NodeImage;
use crate::resource::{Resource, Scope};
use crate::upgrade::UpgradePlan;
use crate::validation::{Validate, ValidationError};

pub const MAXIMUM_STEPS: usize = 10_000;
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
    Upgrade,
    Install,
}

impl fmt::Display for OperationKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Migration => "migration",
            Self::Recovery => "recovery",
            Self::Evacuation => "evacuation",
            Self::Upgrade => "upgrade",
            Self::Install => "install",
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
    Cluster,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum OperationPlan {
    Upgrade(UpgradePlan),
    Install(InstallPlan),
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
    VerifyImage,
    SetClusterImage,
    Evacuate {
        node: NodeName,
    },
    WriteInactiveSlot {
        node: NodeName,
    },
    RebootIntoSlot {
        node: NodeName,
    },
    AwaitHealthy {
        node: NodeName,
    },
    Uncordon {
        node: NodeName,
    },
    Soak,
    DeployMachine {
        node: NodeName,
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
            Self::VerifyImage => "verify-image",
            Self::SetClusterImage => "set-cluster-image",
            Self::Evacuate { .. } => "evacuate",
            Self::WriteInactiveSlot { .. } => "write-inactive-slot",
            Self::RebootIntoSlot { .. } => "reboot-into-slot",
            Self::AwaitHealthy { .. } => "await-healthy",
            Self::Uncordon { .. } => "uncordon",
            Self::Soak => "soak",
            Self::DeployMachine { .. } => "deploy-machine",
        }
    }

    #[must_use]
    pub fn can_be_undone(&self) -> bool {
        matches!(
            self,
            Self::MarkNodeLost { .. }
                | Self::PrepareDestination { .. }
                | Self::CopyMemory { .. }
                | Self::Cordon { .. }
                | Self::VerifyImage
        )
    }

    #[must_use]
    pub fn can_be_abandoned(&self) -> bool {
        matches!(
            self,
            Self::VerifyImage
                | Self::SetClusterImage
                | Self::Evacuate { .. }
                | Self::WriteInactiveSlot { .. }
                | Self::RebootIntoSlot { .. }
                | Self::AwaitHealthy { .. }
                | Self::Uncordon { .. }
                | Self::Soak
                | Self::DeployMachine { .. }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<OperationPlan>,
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

    #[must_use]
    pub fn upgrade(plan: UpgradePlan) -> Self {
        let steps = plan.steps();
        Self {
            plan: Some(OperationPlan::Upgrade(plan)),
            ..Self::new(OperationKind::Upgrade, OperationTarget::Cluster, steps)
        }
    }

    #[must_use]
    pub fn install(plan: InstallPlan) -> Self {
        let steps = plan.steps();
        Self {
            plan: Some(OperationPlan::Install(plan)),
            ..Self::new(OperationKind::Install, OperationTarget::Cluster, steps)
        }
    }

    fn new(kind: OperationKind, target: OperationTarget, steps: Vec<StepSpecification>) -> Self {
        Self { kind, target, steps, is_paused: false, is_cancelled: false, plan: None }
    }

    #[must_use]
    pub fn upgrade_plan(&self) -> Option<&UpgradePlan> {
        match &self.plan {
            Some(OperationPlan::Upgrade(plan)) => Some(plan),
            _ => None,
        }
    }

    #[must_use]
    pub fn install_plan(&self) -> Option<&InstallPlan> {
        match &self.plan {
            Some(OperationPlan::Install(plan)) => Some(plan),
            _ => None,
        }
    }

    #[must_use]
    pub fn image(&self) -> Option<&NodeImage> {
        match &self.plan {
            Some(OperationPlan::Upgrade(plan)) => Some(&plan.image),
            Some(OperationPlan::Install(plan)) => Some(&plan.image),
            None => None,
        }
    }

    fn has_expected_steps(&self) -> bool {
        let has_plan_of_kind = match (&self.kind, &self.plan) {
            (OperationKind::Upgrade, Some(OperationPlan::Upgrade(_)))
            | (OperationKind::Install, Some(OperationPlan::Install(_))) => true,
            (OperationKind::Upgrade | OperationKind::Install, _) | (_, Some(_)) => false,
            _ => true,
        };
        has_plan_of_kind && self.has_steps_of_target()
    }

    fn has_steps_of_target(&self) -> bool {
        match (&self.kind, &self.target) {
            (OperationKind::Upgrade, OperationTarget::Cluster) => {
                self.upgrade_plan().is_some_and(|plan| self.steps == plan.steps())
            }
            (OperationKind::Install, OperationTarget::Cluster) => {
                self.install_plan().is_some_and(|plan| self.steps == plan.steps())
            }
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
        error.check(
            self.steps.len() <= MAXIMUM_STEPS,
            "specification.steps",
            "must have at most 10000 steps",
        );
        error.check(
            self.has_expected_steps(),
            "specification.steps",
            "must be the steps of the operation kind",
        );
        match &self.plan {
            Some(OperationPlan::Upgrade(plan)) => plan.check(&mut error),
            Some(OperationPlan::Install(plan)) => plan.check(&mut error),
            None => {}
        }
        error.into_result()
    }

    fn validate_update(&self, previous: &Self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(self.kind == previous.kind, "specification.kind", "cannot change");
        error.check(self.target == previous.target, "specification.target", "cannot change");
        error.check(self.steps == previous.steps, "specification.steps", "cannot change");
        error.check(self.plan == previous.plan, "specification.plan", "cannot change");
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

    fn image() -> NodeImage {
        NodeImage {
            version: String::from("0.2.0"),
            feature_level: crate::features::FeatureLevel(1),
            digest: "c".repeat(64).parse().unwrap(),
            size_bytes: 1024,
            url: String::from("https://images.example/0.2.0.tar"),
            signature: "0".repeat(128),
        }
    }

    fn upgrade_plan() -> UpgradePlan {
        let inventory = [crate::upgrade::InventoryNode {
            name: node("node-1"),
            zone: None,
            is_core: false,
            runs_target: false,
        }];
        UpgradePlan::upgrade(&inventory, image(), &crate::upgrade::WavePolicy::default())
    }

    #[test]
    fn an_upgrade_must_carry_the_steps_of_its_plan() {
        let upgrade = OperationSpecification::upgrade(upgrade_plan());
        assert_eq!(upgrade.validate(), Ok(()));
        let mut skipped_evacuation = upgrade.clone();
        skipped_evacuation.steps.remove(2);
        assert!(skipped_evacuation.validate().is_err());
        let mut without_plan = upgrade.clone();
        without_plan.plan = None;
        assert!(without_plan.validate().is_err());
        let mut redirected = upgrade.clone();
        if let Some(OperationPlan::Upgrade(plan)) = &mut redirected.plan {
            plan.image.version = String::from("0.3.0");
        }
        assert!(redirected.validate_update(&upgrade).is_err());
        let install =
            OperationSpecification::install(InstallPlan { image: image(), nodes: vec![node("node-0")] });
        assert_eq!(install.validate(), Ok(()));
        let mut planned_migration = OperationSpecification::migration(vm("web-1"), None);
        planned_migration.plan = upgrade.plan.clone();
        assert!(planned_migration.validate().is_err());
    }

    #[test]
    fn upgrade_steps_may_be_abandoned_but_recovery_and_migration_steps_may_not() {
        assert!(
            OperationSpecification::upgrade(upgrade_plan())
                .steps
                .iter()
                .all(StepSpecification::can_be_abandoned)
        );
        assert!(
            !OperationSpecification::recovery(node("node-1"))
                .steps
                .iter()
                .any(StepSpecification::can_be_abandoned)
        );
        assert!(
            !OperationSpecification::migration(vm("web-1"), None)
                .steps
                .iter()
                .any(StepSpecification::can_be_abandoned)
        );
    }

    #[test]
    fn a_recovery_cannot_be_undone_once_storage_is_revoked() {
        let undoable: Vec<bool> = OperationSpecification::recovery(node("node-1"))
            .steps
            .iter()
            .map(StepSpecification::can_be_undone)
            .collect();
        assert_eq!(undoable, vec![true, false, false, false]);
    }
}
