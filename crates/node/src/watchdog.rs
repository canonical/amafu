use std::path::PathBuf;
use std::time::Duration;

use amafu_interfaces::Watchdog;
use amafu_system::watchdog::SoftwareWatchdog;

pub struct KernelWatchdog {
    software: SoftwareWatchdog,
}

impl KernelWatchdog {
    #[must_use]
    pub fn new(module_directory: PathBuf, interval: Duration) -> Self {
        Self { software: SoftwareWatchdog::new(module_directory, interval) }
    }
}

impl Watchdog for KernelWatchdog {
    fn feed(&self) -> std::io::Result<()> {
        self.software.feed()
    }
}
