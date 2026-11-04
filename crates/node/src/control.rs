use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use amafu_interfaces::Watchdog;
use amafu_system::capabilities::{CapabilitySet, keep_only};

use crate::roles::{self, BootServices};
use amafu_node::configuration::NodeConfiguration;

pub const WATCHDOG_SOCKET: &str = "/run/amafu/watchdog.socket";
pub const MODULES_READY_FILE: &str = "/run/amafu/modules-ready";
const MODULES_POLL: Duration = Duration::from_millis(100);
const FEED: &[u8] = b"feed\n";

const CONTROL_CAPABILITIES: CapabilitySet = CapabilitySet::CHOWN
    .union(CapabilitySet::DAC_OVERRIDE)
    .union(CapabilitySet::DAC_READ_SEARCH)
    .union(CapabilitySet::FOWNER)
    .union(CapabilitySet::FSETID)
    .union(CapabilitySet::KILL)
    .union(CapabilitySet::SETGID)
    .union(CapabilitySet::SETUID)
    .union(CapabilitySet::NET_BIND_SERVICE)
    .union(CapabilitySet::NET_BROADCAST)
    .union(CapabilitySet::NET_ADMIN)
    .union(CapabilitySet::NET_RAW)
    .union(CapabilitySet::SYS_ADMIN)
    .union(CapabilitySet::SYS_RESOURCE);

pub struct InitWatchdog {
    socket: PathBuf,
    connection: Mutex<Option<UnixStream>>,
}

impl InitWatchdog {
    #[must_use]
    pub fn new(socket: PathBuf) -> Self {
        Self { socket, connection: Mutex::new(None) }
    }
}

impl Watchdog for InitWatchdog {
    fn feed(&self) -> std::io::Result<()> {
        let mut connection = self.connection.lock().unwrap_or_else(PoisonError::into_inner);
        if connection.is_none() {
            *connection = Some(UnixStream::connect(&self.socket)?);
        }
        let fed = connection.as_mut().map_or(Ok(()), |stream| stream.write_all(FEED));
        if fed.is_err() {
            *connection = None;
        }
        fed
    }
}

async fn modules_ready(marker: PathBuf) {
    while !marker.exists() {
        tokio::time::sleep(MODULES_POLL).await;
    }
}

pub fn drop_capabilities() -> std::io::Result<()> {
    keep_only(CONTROL_CAPABILITIES)
}

pub async fn run(configuration_path: &Path) -> anyhow::Result<()> {
    tracing::info!("the control process runs without the capabilities it does not need");
    crate::reap_orphans_forever();
    let configuration = NodeConfiguration::load(configuration_path)?;
    let boot = BootServices {
        modules: Box::pin(modules_ready(PathBuf::from(MODULES_READY_FILE))),
        watchdog: Arc::new(InitWatchdog::new(PathBuf::from(WATCHDOG_SOCKET))),
    };
    roles::run(configuration, Some(boot)).await
}
