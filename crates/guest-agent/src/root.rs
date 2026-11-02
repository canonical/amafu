use std::ffi::CStr;
use std::fs;
use std::path::Path;

use anyhow::Context;
use rustix::mount::{MountFlags, mount};

pub const CONTAINER_ROOT: &str = "/container";
const LOWER: &str = "/layers/lower";
const UPPER: &str = "/layers/upper";

struct Mount {
    source: &'static str,
    target: &'static str,
    file_system: &'static str,
    flags: MountFlags,
    data: Option<&'static CStr>,
}

const RESTRICTED: MountFlags = MountFlags::NOSUID.union(MountFlags::NODEV).union(MountFlags::NOEXEC);

const CONTAINER_MOUNTS: &[Mount] = &[
    Mount { source: "proc", target: "/container/proc", file_system: "proc", flags: RESTRICTED, data: None },
    Mount { source: "sysfs", target: "/container/sys", file_system: "sysfs", flags: RESTRICTED, data: None },
    Mount {
        source: "devtmpfs",
        target: "/container/dev",
        file_system: "devtmpfs",
        flags: MountFlags::NOSUID,
        data: None,
    },
    Mount {
        source: "tmpfs",
        target: "/container/run",
        file_system: "tmpfs",
        flags: MountFlags::NOSUID.union(MountFlags::NODEV),
        data: Some(c"mode=0755"),
    },
    Mount {
        source: "tmpfs",
        target: "/container/tmp",
        file_system: "tmpfs",
        flags: MountFlags::NOSUID.union(MountFlags::NODEV),
        data: Some(c"mode=1777"),
    },
];

fn mount_at(
    source: &str,
    target: &str,
    file_system: &str,
    flags: MountFlags,
    data: Option<&CStr>,
) -> anyhow::Result<()> {
    fs::create_dir_all(target).with_context(|| format!("creating {target}"))?;
    mount(source, target, file_system, flags, data)
        .with_context(|| format!("mounting {file_system} at {target}"))
}

pub fn prepare(root_device: &Path) -> anyhow::Result<()> {
    let device = root_device.to_string_lossy();
    mount_at(&device, LOWER, "erofs", MountFlags::RDONLY, None)?;
    mount_at("tmpfs", UPPER, "tmpfs", MountFlags::NOSUID.union(MountFlags::NODEV), Some(c"mode=0755"))?;
    for directory in ["/layers/upper/data", "/layers/upper/work"] {
        fs::create_dir_all(directory).with_context(|| format!("creating {directory}"))?;
    }
    mount_at(
        "overlay",
        CONTAINER_ROOT,
        "overlay",
        MountFlags::empty(),
        Some(c"lowerdir=/layers/lower,upperdir=/layers/upper/data,workdir=/layers/upper/work"),
    )?;
    for entry in CONTAINER_MOUNTS {
        mount_at(entry.source, entry.target, entry.file_system, entry.flags, entry.data)?;
    }
    Ok(())
}

pub fn write_name_resolution(domain: Option<&str>, servers: &[std::net::Ipv4Addr]) -> anyhow::Result<()> {
    let mut lines: Vec<String> = domain.map(|domain| format!("search {domain}")).into_iter().collect();
    lines.extend(servers.iter().map(|server| format!("nameserver {server}")));
    let etc = Path::new(CONTAINER_ROOT).join("etc");
    fs::create_dir_all(&etc).context("creating /etc")?;
    fs::write(etc.join("resolv.conf"), lines.join("\n") + "\n").context("writing resolv.conf")
}

pub fn enter() -> anyhow::Result<()> {
    rustix::process::chroot(CONTAINER_ROOT).context("entering the container root")?;
    rustix::process::chdir("/").context("moving to the container root")
}
