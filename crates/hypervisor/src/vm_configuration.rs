use std::fmt::Debug;
use std::os::fd::OwnedFd;

use serde::Serialize;
use serde_json::{Value, json};
use vmm::vm_config::{
    ConsoleConfig, CpusConfig, DebugConsoleConfig, DiskConfig, ExternalFd, MemoryConfig, RngConfig,
    SerialConfig, VmConfig, VsockConfig,
};

use crate::boot::BootFiles;
use crate::plan::VmProcessPlan;

const RANDOM_SOURCE: &str = "src=/dev/urandom";
const DISABLED: &str = "off";

#[derive(Debug, thiserror::Error)]
pub enum ConfigurationError {
    #[error("the {option} option is invalid: {reason}")]
    Option { option: &'static str, reason: String },
    #[error("the VM has {interfaces} interfaces but {taps} taps")]
    TapCount { interfaces: usize, taps: usize },
    #[error("the VM configuration is invalid: {0}")]
    Invalid(String),
}

fn parsed<T: Serialize, E: Debug>(
    option: &'static str,
    outcome: Result<T, E>,
) -> Result<Value, ConfigurationError> {
    let parsed =
        outcome.map_err(|error| ConfigurationError::Option { option, reason: format!("{error:?}") })?;
    serde_json::to_value(parsed)
        .map_err(|error| ConfigurationError::Option { option, reason: error.to_string() })
}

fn payload(boot: &BootFiles) -> Value {
    match boot {
        BootFiles::Kernel { kernel, initramfs, command_line } => {
            json!({"kernel": kernel, "initramfs": initramfs, "cmdline": command_line})
        }
        BootFiles::Firmware { firmware } => json!({"firmware": firmware}),
    }
}

fn listed(values: Vec<Value>) -> Value {
    if values.is_empty() { Value::Null } else { Value::Array(values) }
}

fn document(plan: &VmProcessPlan) -> Result<Value, ConfigurationError> {
    let disks = plan
        .disk_options()
        .iter()
        .map(|disk| parsed("disk", DiskConfig::parse(disk)))
        .collect::<Result<Vec<_>, _>>()?;
    let networks = plan
        .network_devices()
        .into_iter()
        .map(|device| json!({"id": device.id, "mac": device.mac}))
        .collect();
    let mut document = json!({
        "cpus": parsed("cpus", CpusConfig::parse(&plan.cpus_option()))?,
        "memory": parsed("memory", MemoryConfig::parse(&plan.memory_option(), None))?,
        "payload": payload(&plan.boot),
        "disks": listed(disks),
        "net": listed(networks),
        "rng": parsed("rng", RngConfig::parse(RANDOM_SOURCE))?,
        "serial": parsed("serial", SerialConfig::parse(&plan.serial_option()))?,
        "console": parsed("console", ConsoleConfig::parse(DISABLED))?,
        "debug_console": parsed("debug console", DebugConsoleConfig::parse(DISABLED))?,
        "iommu": false,
    });
    if let Some(vsock) = plan.vsock_option() {
        document["vsock"] = parsed("vsock", VsockConfig::parse(&vsock))?;
    }
    if let Some(serial_number) = plan.system_serial_number() {
        document["platform"] = json!({"system_serial_number": serial_number});
    }
    Ok(document)
}

pub fn vm_configuration(plan: &VmProcessPlan, taps: Vec<OwnedFd>) -> Result<VmConfig, ConfigurationError> {
    let mut configuration: VmConfig = serde_json::from_value(document(plan)?)
        .map_err(|error| ConfigurationError::Invalid(error.to_string()))?;
    let networks = configuration.net.as_deref_mut().unwrap_or_default();
    if networks.len() != taps.len() {
        return Err(ConfigurationError::TapCount { interfaces: networks.len(), taps: taps.len() });
    }
    for (network, tap) in networks.iter_mut().zip(taps) {
        network.fds = Some(vec![ExternalFd::from(tap)]);
    }
    configuration.validate().map_err(|error| ConfigurationError::Invalid(format!("{error:?}")))?;
    Ok(configuration)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::fixtures::example_plan;

    fn tap() -> OwnedFd {
        OwnedFd::from(std::io::pipe().unwrap().0)
    }

    #[test]
    fn each_interface_gets_a_stable_id_its_address_and_its_tap() {
        let configuration = vm_configuration(&example_plan(), vec![tap()]).unwrap();
        let networks = configuration.net.unwrap();
        assert_eq!(networks.len(), 1);
        assert_eq!(networks[0].pci_common.id.as_deref(), Some("net0"));
        assert_eq!(networks[0].mac.map(|mac| mac.to_string()).as_deref(), Some("02:00:00:00:00:01"));
        assert_eq!(networks[0].fds.as_ref().map(Vec::len), Some(1));
    }

    #[test]
    fn the_plan_sets_size_disks_firmware_and_the_metadata_seed() {
        let configuration = vm_configuration(&example_plan(), vec![tap()]).unwrap();
        assert_eq!(configuration.cpus.boot_vcpus, 2);
        assert_eq!(configuration.memory.size, 512 * 1024 * 1024);
        assert_eq!(configuration.disks.map(|disks| disks.len()), Some(2));
        assert!(configuration.payload.unwrap().firmware.is_some());
        assert_eq!(
            configuration.platform.unwrap().system_serial_number.as_deref(),
            Some("ds=nocloud;s=http://169.254.169.254/")
        );
    }

    #[test]
    fn every_interface_needs_a_tap() {
        assert!(matches!(
            vm_configuration(&example_plan(), Vec::new()),
            Err(ConfigurationError::TapCount { interfaces: 1, taps: 0 })
        ));
    }
}
