use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::time::Duration;

use amafu_domain::kinds::vm::MigrationReport;
use amafu_domain::{MacAddress, ObjectId, ObjectName};
use serde::{Deserialize, Serialize};

use crate::error::ActionError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Payload {
    Bundle { name: ObjectName },
    Firmware,
    ContainerTemplate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchDisk {
    pub path: PathBuf,
    pub is_read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchInterface {
    pub tap: String,
    pub mac: MacAddress,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmLaunchSpecification {
    pub vm_id: ObjectId,
    pub cpu_count: u16,
    pub memory_mebibytes: u64,
    pub payload: Payload,
    #[serde(default)]
    pub disks: Vec<LaunchDisk>,
    #[serde(default)]
    pub network_interfaces: Vec<LaunchInterface>,
    #[serde(default)]
    pub has_guest_agent: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_seed: Option<String>,
}

impl VmLaunchSpecification {
    #[must_use]
    pub fn of_bundle(vm_id: ObjectId, cpu_count: u16, memory_mebibytes: u64, bundle: ObjectName) -> Self {
        Self {
            vm_id,
            cpu_count,
            memory_mebibytes,
            payload: Payload::Bundle { name: bundle },
            disks: Vec::new(),
            network_interfaces: Vec::new(),
            has_guest_agent: false,
            metadata_seed: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstanceState {
    Booting,
    Running,
    Exited,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum InstanceMigration {
    Receiving,
    Sending,
    Sent { report: MigrationReport },
    Failed { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationTarget {
    pub node_address: Ipv4Addr,
    pub downtime_milliseconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningInstance {
    pub vm_id: ObjectId,
    pub state: InstanceState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration: Option<InstanceMigration>,
}

impl RunningInstance {
    #[must_use]
    pub fn new(vm_id: ObjectId, state: InstanceState) -> Self {
        Self { vm_id, state, failure: None, migration: None }
    }

    #[must_use]
    pub fn migrating(mut self, migration: Option<InstanceMigration>) -> Self {
        self.migration = migration;
        self
    }
}

#[async_trait::async_trait]
pub trait Hypervisor: Send + Sync {
    async fn start(&self, launch: &VmLaunchSpecification) -> Result<(), ActionError>;

    async fn stop(&self, vm_id: &ObjectId, grace_period: Duration) -> Result<(), ActionError>;

    async fn running(&self) -> Result<Vec<RunningInstance>, ActionError>;

    async fn receive(&self, launch: &VmLaunchSpecification) -> Result<(), ActionError>;

    async fn send(&self, vm_id: &ObjectId, target: &MigrationTarget) -> Result<(), ActionError>;
}
