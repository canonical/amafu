use std::ffi::CStr;
use std::fs;
use std::path::Path;

use rustix::mount::{MountFlags, mount};

#[derive(Debug, thiserror::Error)]
pub enum EarlyBootError {
    #[error("mounting {target} failed: {cause}")]
    Mount { target: String, cause: std::io::Error },
    #[error("creating {path} failed: {cause}")]
    Directory { path: String, cause: std::io::Error },
    #[error("setting the hostname failed: {0}")]
    Hostname(std::io::Error),
}

struct Mount {
    source: &'static str,
    target: &'static str,
    file_system: &'static str,
    flags: MountFlags,
    data: Option<&'static CStr>,
    is_optional: bool,
}

const RESTRICTED: MountFlags = MountFlags::NOSUID.union(MountFlags::NODEV).union(MountFlags::NOEXEC);

const ESSENTIAL_MOUNTS: &[Mount] = &[
    Mount {
        source: "proc",
        target: "/proc",
        file_system: "proc",
        flags: RESTRICTED,
        data: None,
        is_optional: false,
    },
    Mount {
        source: "sysfs",
        target: "/sys",
        file_system: "sysfs",
        flags: RESTRICTED,
        data: None,
        is_optional: false,
    },
    Mount {
        source: "devtmpfs",
        target: "/dev",
        file_system: "devtmpfs",
        flags: MountFlags::NOSUID,
        data: Some(c"mode=0755"),
        is_optional: false,
    },
    Mount {
        source: "tmpfs",
        target: "/run",
        file_system: "tmpfs",
        flags: MountFlags::NOSUID.union(MountFlags::NODEV),
        data: Some(c"mode=0755"),
        is_optional: false,
    },
    Mount {
        source: "tmpfs",
        target: "/tmp",
        file_system: "tmpfs",
        flags: RESTRICTED,
        data: None,
        is_optional: false,
    },
    Mount {
        source: "cgroup2",
        target: "/sys/fs/cgroup",
        file_system: "cgroup2",
        flags: RESTRICTED,
        data: None,
        is_optional: false,
    },
    Mount {
        source: "efivarfs",
        target: "/sys/firmware/efi/efivars",
        file_system: "efivarfs",
        flags: RESTRICTED.union(MountFlags::RDONLY),
        data: None,
        is_optional: true,
    },
];

fn create_directory(path: &Path) -> Result<(), EarlyBootError> {
    fs::create_dir_all(path)
        .map_err(|cause| EarlyBootError::Directory { path: path.display().to_string(), cause })
}

fn mount_failure(target: &Path) -> impl Fn(rustix::io::Errno) -> EarlyBootError + '_ {
    move |error| EarlyBootError::Mount { target: target.display().to_string(), cause: error.into() }
}

pub fn mount_essential_filesystems() -> Result<(), EarlyBootError> {
    for entry in ESSENTIAL_MOUNTS {
        if !Path::new(entry.target).exists() {
            if entry.is_optional {
                tracing::info!(target = entry.target, "an optional file system has no mount point");
                continue;
            }
            create_directory(Path::new(entry.target))?;
        }
        match mount(entry.source, entry.target, entry.file_system, entry.flags, entry.data) {
            Ok(()) | Err(rustix::io::Errno::BUSY) => {}
            Err(error) if entry.is_optional => {
                tracing::info!(target = entry.target, %error, "an optional file system is unavailable");
            }
            Err(error) => {
                return Err(EarlyBootError::Mount { target: entry.target.to_owned(), cause: error.into() });
            }
        }
    }
    Ok(())
}

pub fn mount_data_partition(device: &Path, target: &Path) -> Result<(), EarlyBootError> {
    create_directory(target)?;
    mount(device, target, "ext4", MountFlags::NOSUID.union(MountFlags::NODEV), None)
        .map_err(mount_failure(target))
}

pub fn mount_boot_partition(device: &Path, target: &Path) -> Result<(), EarlyBootError> {
    create_directory(target)?;
    let flags = MountFlags::NOSUID.union(MountFlags::NODEV).union(MountFlags::RDONLY);
    mount(device, target, "vfat", flags, Some(c"fmask=0022,dmask=0022")).map_err(mount_failure(target))
}

pub fn set_hostname(name: &str) -> Result<(), EarlyBootError> {
    rustix::system::sethostname(name.as_bytes()).map_err(|error| EarlyBootError::Hostname(error.into()))
}
