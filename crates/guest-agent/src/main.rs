mod dhcp_client;
mod root;
mod server;
mod workload;

use std::path::Path;
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rustix::system::RebootCommand;

use crate::workload::Workload;

const MODULES_DIRECTORY: &str = "/usr/lib/amafu/modules";
const MODULE_LIST: &str = "/usr/lib/amafu/modules/load-order";
const ROOT_DEVICE: &str = "/dev/vda";
const REAP_INTERVAL: Duration = Duration::from_millis(200);

async fn run() -> anyhow::Result<()> {
    amafu_system::mount_essential_filesystems()?;
    let uptime = amafu_system::host::uptime_seconds(Path::new("/proc"));
    tracing::info!(?uptime, "the guest agent is starting");
    amafu_system::modules::load_listed_modules(Path::new(MODULES_DIRECTORY), Path::new(MODULE_LIST))?;
    root::prepare(Path::new(ROOT_DEVICE))?;
    match dhcp_client::configure(None).await {
        Ok(configured) => {
            tracing::info!(interface = %configured.interface, address = %configured.address, "the network is configured");
            root::write_name_resolution(
                configured.lease.domain_name.as_deref(),
                &configured.lease.dns_servers,
            )?;
        }
        Err(error) => tracing::warn!(error = format!("{error:#}"), "the container has no network"),
    }
    root::enter()?;
    let workload = Arc::new(Mutex::new(Workload::default()));
    let reaper = Arc::clone(&workload);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(REAP_INTERVAL);
        loop {
            interval.tick().await;
            reaper.lock().unwrap_or_else(PoisonError::into_inner).reap();
        }
    });
    tracing::info!(uptime = ?amafu_system::host::uptime_seconds(Path::new("/proc")), "the guest is ready");
    server::serve(workload).await
}

fn main() -> ExitCode {
    tracing_subscriber::fmt().with_target(false).init();
    let outcome = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(anyhow::Error::from)
        .and_then(|runtime| runtime.block_on(run()));
    if let Err(error) = outcome {
        tracing::error!(error = format!("{error:#}"), "the guest agent failed; powering off");
    }
    rustix::fs::sync();
    let _ = rustix::system::reboot(RebootCommand::PowerOff);
    ExitCode::FAILURE
}
