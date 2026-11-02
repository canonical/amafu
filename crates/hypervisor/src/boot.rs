use std::path::{Path, PathBuf};

use amafu_interfaces::Payload;
use serde::{Deserialize, Serialize};

use crate::images::{ImageError, find_image};

pub const CONTAINER_COMMAND_LINE: &str = "console=ttyS0 quiet panic=-1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootSources {
    pub bundles_directory: PathBuf,
    pub firmware: PathBuf,
    pub container_kernel: PathBuf,
    pub container_initramfs: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum BootFiles {
    Kernel { kernel: PathBuf, initramfs: Option<PathBuf>, command_line: String },
    Firmware { firmware: PathBuf },
}

impl BootFiles {
    #[must_use]
    pub fn paths(&self) -> Vec<PathBuf> {
        match self {
            Self::Kernel { kernel, initramfs, .. } => {
                std::iter::once(kernel.clone()).chain(initramfs.iter().cloned()).collect()
            }
            Self::Firmware { firmware } => vec![firmware.clone()],
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BootError {
    #[error(transparent)]
    Image(#[from] ImageError),
    #[error("{0} is not installed on this node")]
    Missing(PathBuf),
}

fn installed(path: &Path) -> Result<PathBuf, BootError> {
    if path.is_file() { Ok(path.to_path_buf()) } else { Err(BootError::Missing(path.to_path_buf())) }
}

impl BootSources {
    pub fn resolve(&self, payload: &Payload) -> Result<BootFiles, BootError> {
        match payload {
            Payload::Bundle { name } => {
                let bundle = find_image(&self.bundles_directory, name)?;
                Ok(BootFiles::Kernel {
                    kernel: bundle.kernel,
                    initramfs: bundle.initramfs,
                    command_line: bundle.command_line,
                })
            }
            Payload::Firmware => Ok(BootFiles::Firmware { firmware: installed(&self.firmware)? }),
            Payload::ContainerTemplate => Ok(BootFiles::Kernel {
                kernel: installed(&self.container_kernel)?,
                initramfs: Some(installed(&self.container_initramfs)?),
                command_line: CONTAINER_COMMAND_LINE.to_owned(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources(root: &std::path::Path) -> BootSources {
        BootSources {
            bundles_directory: root.join("bundles"),
            firmware: root.join("CLOUDHV.fd"),
            container_kernel: root.join("vmlinuz"),
            container_initramfs: root.join("container.img"),
        }
    }

    #[test]
    fn firmware_and_container_payloads_need_their_files_installed() {
        let root = tempfile::tempdir().unwrap();
        let sources = sources(root.path());
        assert!(matches!(sources.resolve(&Payload::Firmware), Err(BootError::Missing(_))));
        assert!(matches!(sources.resolve(&Payload::ContainerTemplate), Err(BootError::Missing(_))));
        for file in ["CLOUDHV.fd", "vmlinuz", "container.img"] {
            std::fs::write(root.path().join(file), b"").unwrap();
        }
        assert_eq!(
            sources.resolve(&Payload::Firmware).unwrap(),
            BootFiles::Firmware { firmware: root.path().join("CLOUDHV.fd") }
        );
        let container = sources.resolve(&Payload::ContainerTemplate).unwrap();
        assert_eq!(container.paths(), vec![root.path().join("vmlinuz"), root.path().join("container.img")]);
    }
}
