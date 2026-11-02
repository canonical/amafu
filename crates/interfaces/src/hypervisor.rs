use std::time::Duration;

use amafu_domain::{ObjectId, ObjectName};
use serde::{Deserialize, Serialize};

use crate::error::ActionError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmLaunchSpecification {
    pub vm_id: ObjectId,
    pub cpu_count: u16,
    pub memory_mebibytes: u64,
    pub image: ObjectName,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstanceState {
    Booting,
    Running,
    Exited,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningInstance {
    pub vm_id: ObjectId,
    pub state: InstanceState,
}

#[async_trait::async_trait]
pub trait Hypervisor: Send + Sync {
    async fn start(&self, launch: &VmLaunchSpecification) -> Result<(), ActionError>;

    async fn stop(&self, vm_id: &ObjectId, grace_period: Duration) -> Result<(), ActionError>;

    async fn running(&self) -> Result<Vec<RunningInstance>, ActionError>;
}
