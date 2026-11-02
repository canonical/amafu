use amafu_domain::ObjectId;
use amafu_interfaces::InstanceState;
use serde::{Deserialize, Serialize};

pub const INSTANCES_PATH: &str = "/diagnostics/instances";
pub const BOOT_PATH: &str = "/diagnostics/boot";
pub const HYPERVISOR_CONTRACT_PATH: &str = "/diagnostics/contracts/hypervisor";
pub const AUDIT_PATH: &str = "/diagnostics/audit";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeInstance {
    pub vm_id: ObjectId,
    pub state: InstanceState,
    #[serde(default)]
    pub console_tail: String,
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
