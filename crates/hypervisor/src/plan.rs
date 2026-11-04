use std::io::Read;
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};

use amafu_interfaces::VmLaunchSpecification;
use serde::{Deserialize, Serialize};

use crate::boot::BootFiles;

pub const GUEST_CONTEXT_IDENTIFIER: u32 = 3;
pub const TAP_DESCRIPTOR_BASE: RawFd = 200;
const QCOW2_MAGIC: &[u8; 4] = b"QFI\xfb";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiskFormat {
    Raw,
    Qcow2,
}

impl DiskFormat {
    pub fn of(path: &Path) -> std::io::Result<Self> {
        let mut magic = [0_u8; 4];
        let mut file = std::fs::File::open(path)?;
        let is_qcow2 = file.read_exact(&mut magic).is_ok() && &magic == QCOW2_MAGIC;
        Ok(if is_qcow2 { Self::Qcow2 } else { Self::Raw })
    }

    fn image_type(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Qcow2 => "qcow2",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmProcessPlan {
    pub launch: VmLaunchSpecification,
    pub boot: BootFiles,
    pub disk_formats: Vec<DiskFormat>,
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
            .zip(&self.disk_formats)
            .map(|(disk, format)| {
                format!(
                    "path={},readonly={},image_type={},backing_files=on",
                    disk.path.display(),
                    toggle(disk.is_read_only),
                    format.image_type()
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
            disk_formats: vec![DiskFormat::Qcow2, DiskFormat::Raw],
            serial_log: PathBuf::from("/run/vm/serial.log"),
            vsock_socket: PathBuf::from("/run/vm/vsock.socket"),
        }
    }

    #[test]
    fn qcow2_images_are_recognized_by_their_magic() {
        let directory = tempfile::tempdir().unwrap();
        let qcow2 = directory.path().join("disk.qcow2");
        std::fs::write(&qcow2, b"QFI\xfb\x00\x00\x00\x03").unwrap();
        let raw = directory.path().join("root.erofs");
        std::fs::write(&raw, b"\x00").unwrap();
        assert_eq!(DiskFormat::of(&qcow2).unwrap(), DiskFormat::Qcow2);
        assert_eq!(DiskFormat::of(&raw).unwrap(), DiskFormat::Raw);
    }

    #[test]
    fn devices_are_described_in_cloud_hypervisor_syntax() {
        let plan = plan();
        assert_eq!(
            plan.disk_options(),
            vec![
                "path=/volumes/root.qcow2,readonly=off,image_type=qcow2,backing_files=on",
                "path=/images/seed.raw,readonly=on,image_type=raw,backing_files=on"
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
