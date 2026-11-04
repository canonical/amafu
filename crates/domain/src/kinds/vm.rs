use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

use crate::addressing::MacAddress;
use crate::kinds::node::CpuBaseline;
use crate::name::{NodeName, ObjectName};
use crate::resource::{Resource, Scope};
use crate::validation::{Validate, ValidationError};

pub const MAXIMUM_CPU_COUNT: u16 = 256;
pub const MINIMUM_MEMORY_MEBIBYTES: u64 = 64;
pub const MAXIMUM_MEMORY_MEBIBYTES: u64 = 4 * 1024 * 1024;
pub const MAXIMUM_VOLUMES: usize = 8;
pub const MAXIMUM_NETWORK_INTERFACES: usize = 4;
pub const MAXIMUM_SECURITY_GROUPS_PER_INTERFACE: usize = 8;
pub const MAXIMUM_USER_DATA_BYTES: usize = 64 * 1024;
pub const MAXIMUM_COMMAND_ARGUMENTS: usize = 256;
pub const MAXIMUM_ENVIRONMENT_VARIABLES: usize = 256;
pub const CONTAINER_CPU_COUNT: u16 = 1;
pub const CONTAINER_MEMORY_MEBIBYTES: u64 = 256;
pub const MAXIMUM_PRIORITY: i32 = 1000;

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
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum BootSource {
    Kernel {
        bundle: ObjectName,
    },
    Disk,
    Container {
        image: ObjectName,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        command: Vec<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        environment: BTreeMap<String, String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkInterfaceSpecification {
    pub network: ObjectName,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub security_groups: Vec<ObjectName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<Ipv4Addr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmSpecification {
    pub cpu_count: u16,
    pub memory_mebibytes: u64,
    pub boot: BootSource,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<ObjectName>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network_interfaces: Vec<NetworkInterfaceSpecification>,
    #[serde(default)]
    pub power_state: PowerState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_data: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub priority: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anti_affinity_group: Option<ObjectName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_cpu_baseline: Option<CpuBaseline>,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(value: &i32) -> bool {
    *value == 0
}

impl VmSpecification {
    #[must_use]
    pub fn small() -> Self {
        Self {
            cpu_count: 1,
            memory_mebibytes: 256,
            boot: BootSource::Kernel {
                bundle: ObjectName::new("test-guest").unwrap_or_else(|_| unreachable!()),
            },
            volumes: Vec::new(),
            network_interfaces: Vec::new(),
            power_state: PowerState::Running,
            user_data: None,
            priority: 0,
            anti_affinity_group: None,
            minimum_cpu_baseline: None,
        }
    }

    #[must_use]
    pub fn container(image: ObjectName) -> Self {
        Self {
            cpu_count: CONTAINER_CPU_COUNT,
            memory_mebibytes: CONTAINER_MEMORY_MEBIBYTES,
            boot: BootSource::Container { image, command: Vec::new(), environment: BTreeMap::new() },
            ..Self::small()
        }
    }

    #[must_use]
    pub fn should_run(&self) -> bool {
        self.power_state == PowerState::Running
    }

    #[must_use]
    pub fn is_container(&self) -> bool {
        matches!(self.boot, BootSource::Container { .. })
    }

    #[must_use]
    pub fn container_image(&self) -> Option<&ObjectName> {
        match &self.boot {
            BootSource::Container { image, .. } => Some(image),
            BootSource::Kernel { .. } | BootSource::Disk => None,
        }
    }

    fn validate_boot(&self, error: &mut ValidationError) {
        match &self.boot {
            BootSource::Disk => error.check(
                !self.volumes.is_empty(),
                "specification.volumes",
                "a VM that boots from disk needs at least one volume; the first one is the boot disk",
            ),
            BootSource::Container { command, environment, .. } => {
                error.check(
                    command.len() <= MAXIMUM_COMMAND_ARGUMENTS,
                    "specification.boot.command",
                    "must hold at most 256 arguments",
                );
                error.check(
                    environment.len() <= MAXIMUM_ENVIRONMENT_VARIABLES,
                    "specification.boot.environment",
                    "must hold at most 256 variables",
                );
                let are_names_valid = environment.keys().all(|name| {
                    !name.is_empty()
                        && !name.starts_with(|character: char| character.is_ascii_digit())
                        && name.chars().all(|character| character.is_ascii_alphanumeric() || character == '_')
                });
                error.check(
                    are_names_valid,
                    "specification.boot.environment",
                    "names must be letters, digits and _",
                );
                let is_text_clean =
                    command.iter().chain(environment.values()).all(|text| !text.contains('\0'));
                error.check(is_text_clean, "specification.boot", "arguments and values must not contain NUL");
            }
            BootSource::Kernel { .. } => {}
        }
    }

    fn validate_attachments(&self, error: &mut ValidationError) {
        error.check(
            self.volumes.len() <= MAXIMUM_VOLUMES,
            "specification.volumes",
            "must hold at most 8 volumes",
        );
        let distinct: BTreeSet<&ObjectName> = self.volumes.iter().collect();
        error.check(
            distinct.len() == self.volumes.len(),
            "specification.volumes",
            "must not repeat a volume",
        );
        error.check(
            self.network_interfaces.len() <= MAXIMUM_NETWORK_INTERFACES,
            "specification.network_interfaces",
            "must hold at most 4 interfaces",
        );
        for (index, interface) in self.network_interfaces.iter().enumerate() {
            error.check(
                interface.security_groups.len() <= MAXIMUM_SECURITY_GROUPS_PER_INTERFACE,
                &format!("specification.network_interfaces[{index}].security_groups"),
                "must hold at most 8 security groups",
            );
        }
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
        error.check(
            self.user_data.as_ref().is_none_or(|data| data.len() <= MAXIMUM_USER_DATA_BYTES),
            "specification.user_data",
            "must be at most 64 KiB",
        );
        error.check(
            (-MAXIMUM_PRIORITY..=MAXIMUM_PRIORITY).contains(&self.priority),
            "specification.priority",
            "must be between -1000 and 1000",
        );
        self.validate_boot(&mut error);
        self.validate_attachments(&mut error);
        error.into_result()
    }

    fn validate_update(&self, previous: &Self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(self.boot == previous.boot, "specification.boot", "cannot change");
        error.check(self.volumes == previous.volumes, "specification.volumes", "cannot change");
        error.check(
            self.network_interfaces == previous.network_interfaces,
            "specification.network_interfaces",
            "cannot change",
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterfaceStatus {
    pub network: ObjectName,
    pub mac: MacAddress,
    pub address: Ipv4Addr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MigrationPhase {
    Preparing,
    DestinationReady,
    Copying,
    Copied,
    SwitchedOver,
    Cleaned,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MigrationReport {
    pub total_bytes: u64,
    pub final_round_bytes: u64,
    pub duration_milliseconds: u64,
    pub downtime_milliseconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmMigration {
    pub source: NodeName,
    pub destination: NodeName,
    pub phase: MigrationPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<MigrationReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl VmMigration {
    #[must_use]
    pub fn is_active(&self) -> bool {
        !matches!(self.phase, MigrationPhase::Cleaned | MigrationPhase::Failed)
    }

    #[must_use]
    pub fn involves(&self, node: &NodeName) -> bool {
        self.is_active() && (&self.source == node || &self.destination == node)
    }

    #[must_use]
    pub fn at(&self, phase: MigrationPhase) -> Self {
        Self { phase, ..self.clone() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct VmStatus {
    pub phase: VmPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<NodeName>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interfaces: Vec<InterfaceStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration: Option<VmMigration>,
}

impl VmStatus {
    #[must_use]
    pub fn placed_on(&self, node: &NodeName) -> Self {
        Self { node: Some(node.clone()), ..self.clone() }
    }

    #[must_use]
    pub fn active_migration(&self) -> Option<&VmMigration> {
        self.migration.as_ref().filter(|migration| migration.is_active())
    }

    #[must_use]
    pub fn is_migrating(&self) -> bool {
        self.active_migration().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(value: &str) -> ObjectName {
        ObjectName::new(value).unwrap()
    }

    #[test]
    fn the_small_specification_is_valid() {
        assert_eq!(VmSpecification::small().validate(), Ok(()));
        assert_eq!(VmSpecification::container(name("busybox")).validate(), Ok(()));
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
    fn booting_from_disk_needs_a_volume() {
        let mut specification = VmSpecification { boot: BootSource::Disk, ..VmSpecification::small() };
        assert!(specification.validate().is_err());
        specification.volumes = vec![name("root")];
        assert_eq!(specification.validate(), Ok(()));
        specification.volumes.push(name("root"));
        assert!(specification.validate().is_err());
    }

    #[test]
    fn container_environment_names_are_checked() {
        let mut specification = VmSpecification::container(name("busybox"));
        if let BootSource::Container { environment, .. } = &mut specification.boot {
            environment.insert(String::from("1BAD"), String::from("value"));
        }
        assert!(specification.validate().is_err());
    }

    #[test]
    fn attachments_cannot_change_but_size_can() {
        let previous = VmSpecification::small();
        let mut resized = previous.clone();
        resized.cpu_count = 2;
        assert_eq!(resized.validate_update(&previous), Ok(()));
        let mut rewired = previous.clone();
        rewired.network_interfaces.push(NetworkInterfaceSpecification {
            network: name("default"),
            security_groups: Vec::new(),
            address: None,
        });
        assert!(rewired.validate_update(&previous).is_err());
    }

    #[test]
    fn unknown_fields_are_refused() {
        let body =
            r#"{"cpu_count":1,"memory_mebibytes":256,"boot":{"type":"disk"},"volumes":["root"],"gpu":true}"#;
        assert!(serde_json::from_str::<VmSpecification>(body).is_err());
    }

    #[test]
    fn phases_follow_the_lifecycle() {
        assert!(VmPhase::Pending.can_become(VmPhase::Scheduled));
        assert!(VmPhase::Scheduled.can_become(VmPhase::Starting));
        assert!(VmPhase::Starting.can_become(VmPhase::Running));
        assert!(VmPhase::Running.can_become(VmPhase::Stopping));
        assert!(VmPhase::Stopping.can_become(VmPhase::Stopped));
        assert!(!VmPhase::Pending.can_become(VmPhase::Running));
        assert!(!VmPhase::Stopping.can_become(VmPhase::Running));
    }
}
