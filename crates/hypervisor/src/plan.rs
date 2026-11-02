use std::path::PathBuf;

use amafu_interfaces::VmLaunchSpecification;
use serde::{Deserialize, Serialize};

use crate::images::ImageBundle;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmProcessPlan {
    pub launch: VmLaunchSpecification,
    pub image: ImageBundle,
    pub serial_log: PathBuf,
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
}
