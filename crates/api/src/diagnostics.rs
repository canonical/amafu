use std::net::Ipv4Addr;
use std::path::PathBuf;

use amafu_domain::{MacAddress, ObjectId};
use amafu_interfaces::{InstanceState, WorkloadState};
use serde::{Deserialize, Serialize};

pub const INSTANCES_PATH: &str = "/diagnostics/instances";
pub const BOOT_PATH: &str = "/diagnostics/boot";
pub const HYPERVISOR_CONTRACT_PATH: &str = "/diagnostics/contracts/hypervisor";
pub const AUDIT_PATH: &str = "/diagnostics/audit";
pub const NETWORK_PATH: &str = "/diagnostics/network";
pub const ATTACHMENTS_PATH: &str = "/diagnostics/attachments";
pub const SANDBOX_PATH: &str = "/diagnostics/sandbox";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeInstance {
    pub vm_id: ObjectId,
    pub state: InstanceState,
    #[serde(default)]
    pub console_tail: String,
    #[serde(default)]
    pub process_log_tail: String,
    #[serde(default)]
    pub disks: Vec<PathBuf>,
    #[serde(default)]
    pub mac_addresses: Vec<MacAddress>,
    #[serde(default)]
    pub workload: Option<WorkloadState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodePort {
    pub vm_id: ObjectId,
    pub index: u8,
    pub segment: String,
    pub mac: MacAddress,
    pub address: Ipv4Addr,
    pub tap: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeNetworkFacts {
    pub ports: Vec<NodePort>,
    pub ruleset: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeAttachment {
    pub volume: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxCheck {
    pub name: String,
    pub is_blocked: bool,
    pub outcome: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BootFacts {
    pub is_secure_boot_enabled: bool,
    pub is_tpm_present: bool,
    pub is_kvm_available: bool,
    pub seconds_from_kernel_start_to_serving: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractOutcome {
    pub violation: Option<String>,
}
