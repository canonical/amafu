use std::os::fd::RawFd;
use std::path::PathBuf;

use amafu_interfaces::VmLaunchSpecification;
use serde::{Deserialize, Serialize};

use crate::boot::BootFiles;

pub const GUEST_CONTEXT_IDENTIFIER: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmProcessPlan {
    pub launch: VmLaunchSpecification,
    pub boot: BootFiles,
    pub serial_log: PathBuf,
    pub vsock_socket: PathBuf,
}

fn toggle(is_on: bool) -> &'static str {
    if is_on { "on" } else { "off" }
}

impl VmProcessPlan {
    #[must_use]
    pub fn cpus_option(&self) -> String {
        format!("boot={}", self.launch.cpu_count)
    }

    #[must_use]
    pub fn memory_option(&self) -> String {
        format!("size={}M", self.launch.memory_mebibytes)
    }

    #[must_use]
    pub fn serial_option(&self) -> String {
        format!("file={}", self.serial_log.display())
    }

    #[must_use]
    pub fn disk_options(&self) -> Vec<String> {
        self.launch
            .disks
            .iter()
            .map(|disk| {
                format!(
                    "path={},readonly={},backing_files=on",
                    disk.path.display(),
                    toggle(disk.is_read_only)
                )
            })
            .collect()
    }

    #[must_use]
    pub fn network_options(&self, tap_descriptors: &[RawFd]) -> Vec<String> {
        self.launch
            .network_interfaces
            .iter()
            .zip(tap_descriptors)
            .map(|(interface, descriptor)| format!("fd=[{descriptor}],mac={}", interface.mac))
            .collect()
    }

    #[must_use]
    pub fn vsock_option(&self) -> Option<String> {
        self.launch
            .has_guest_agent
            .then(|| format!("cid={GUEST_CONTEXT_IDENTIFIER},socket={}", self.vsock_socket.display()))
    }

    #[must_use]
    pub fn platform_option(&self) -> Option<String> {
        self.launch.metadata_seed.as_ref().map(|seed| format!("system_serial_number={seed}"))
    }
}

#[cfg(test)]
mod tests {
    use amafu_domain::{MacAddress, ObjectId, ObjectName};
    use amafu_interfaces::{LaunchDisk, LaunchInterface, Payload};

    use super::*;

    fn plan() -> VmProcessPlan {
        let mut launch = VmLaunchSpecification::of_bundle(
            ObjectId::from_uuid(uuid::Uuid::from_u128(7)),
            2,
            512,
            ObjectName::new("test-guest").unwrap(),
        );
        launch.payload = Payload::Firmware;
        launch.disks = vec![
            LaunchDisk { path: PathBuf::from("/volumes/root.qcow2"), is_read_only: false },
            LaunchDisk { path: PathBuf::from("/images/seed.raw"), is_read_only: true },
        ];
        launch.network_interfaces = vec![LaunchInterface {
            tap: String::from("amt000000000001"),
            mac: MacAddress::new([2, 0, 0, 0, 0, 1]),
        }];
        launch.metadata_seed = Some(String::from("ds=nocloud;s=http://169.254.169.254/"));
        VmProcessPlan {
            launch,
            boot: BootFiles::Firmware { firmware: PathBuf::from("/usr/share/cloud-hypervisor/CLOUDHV.fd") },
            serial_log: PathBuf::from("/run/vm/serial.log"),
            vsock_socket: PathBuf::from("/run/vm/vsock.socket"),
        }
    }

    #[test]
    fn devices_are_described_in_cloud_hypervisor_syntax() {
        let plan = plan();
        assert_eq!(
            plan.disk_options(),
            vec![
                "path=/volumes/root.qcow2,readonly=off,backing_files=on",
                "path=/images/seed.raw,readonly=on,backing_files=on"
            ]
        );
        assert_eq!(plan.network_options(&[5]), vec!["fd=[5],mac=02:00:00:00:00:01"]);
        assert_eq!(
            plan.platform_option().unwrap(),
            "system_serial_number=ds=nocloud;s=http://169.254.169.254/"
        );
        assert_eq!(plan.vsock_option(), None);
    }
}
