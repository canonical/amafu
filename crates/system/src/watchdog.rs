use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use rustix::io::Errno;

const DEVICE: &str = "/dev/watchdog";
const MODULE: &str = "softdog.ko";

pub struct SoftwareWatchdog {
    module_directory: PathBuf,
    interval: Duration,
    device: Mutex<Option<File>>,
}

impl SoftwareWatchdog {
    #[must_use]
    pub fn new(module_directory: PathBuf, interval: Duration) -> Self {
        Self { module_directory, interval, device: Mutex::new(None) }
    }

    fn arm(&self) -> std::io::Result<File> {
        let module = File::open(self.module_directory.join(MODULE))?;
        let parameters = CString::new(margin_parameter(self.interval)).map_err(std::io::Error::other)?;
        match rustix::system::finit_module(&module, &parameters, 0) {
            Ok(()) | Err(Errno::EXIST) => {}
            Err(error) => return Err(error.into()),
        }
        OpenOptions::new().write(true).open(DEVICE)
    }
}

fn margin_parameter(interval: Duration) -> String {
    format!("soft_margin={}", interval.as_secs().max(1))
}

impl SoftwareWatchdog {
    pub fn feed(&self) -> std::io::Result<()> {
        let mut device = self.device.lock().unwrap_or_else(PoisonError::into_inner);
        if device.is_none() {
            *device = Some(self.arm()?);
        }
        match device.as_mut() {
            Some(file) => file.write_all(b"k"),
            None => Err(std::io::Error::other("the watchdog device is not open")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_margin_is_given_in_whole_seconds() {
        assert_eq!(margin_parameter(Duration::from_secs(8)), "soft_margin=8");
        assert_eq!(margin_parameter(Duration::from_millis(300)), "soft_margin=1");
    }
}
