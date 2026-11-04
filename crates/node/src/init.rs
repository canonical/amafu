use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use amafu_interfaces::Watchdog;
use tokio::io::AsyncBufReadExt;
use tokio::net::UnixListener;
use tokio::sync::mpsc;

use crate::boot;
use crate::control::{FEED_REQUEST, INIT_SOCKET, MODULES_READY_FILE, RESTART_REQUEST};
use crate::watchdog::KernelWatchdog;

const CONTROL_COMMAND: &str = "control";
const CONFIGURATION_ARGUMENT: &str = "--configuration";
const INITIAL_RESTART_DELAY: Duration = Duration::from_secs(1);
const MAXIMUM_RESTART_DELAY: Duration = Duration::from_secs(30);
const REAP_INTERVAL: Duration = Duration::from_millis(200);
const HEALTHY_RUN: Duration = Duration::from_secs(60);

fn restart_node() {
    tracing::warn!("the control process asked to restart the node");
    if let Err(error) = amafu_system::reboot() {
        tracing::error!(%error, "restarting the node failed");
    }
}

fn answer_control_requests(watchdog: Arc<KernelWatchdog>) -> std::io::Result<()> {
    let _ = std::fs::remove_file(INIT_SOCKET);
    let listener = UnixListener::bind(INIT_SOCKET)?;
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            let watchdog = Arc::clone(&watchdog);
            tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(stream).lines();
                while let Ok(Some(request)) = lines.next_line().await {
                    match request.as_bytes() {
                        FEED_REQUEST => {
                            if let Err(error) = watchdog.feed() {
                                tracing::error!(%error, "feeding the watchdog failed");
                            }
                        }
                        RESTART_REQUEST => restart_node(),
                        _ => tracing::warn!(request, "the control process sent an unknown request"),
                    }
                }
            });
        }
    });
    Ok(())
}

fn mark_modules_ready(modules: boot::ModuleLoading) {
    tokio::spawn(async move {
        if let Err(error) = modules.await {
            tracing::error!(%error, "loading kernel modules stopped");
        }
        if let Err(error) = std::fs::write(MODULES_READY_FILE, b"") {
            tracing::error!(%error, "marking the kernel modules loaded failed");
        }
    });
}

fn reap(ended: mpsc::UnboundedSender<i32>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(REAP_INTERVAL);
        loop {
            interval.tick().await;
            for reaped in amafu_system::reap_orphans() {
                let _ = ended.send(reaped.process_id);
            }
        }
    });
}

async fn supervise_control(
    configuration: &Path,
    mut ended: mpsc::UnboundedReceiver<i32>,
) -> anyhow::Result<()> {
    let executable = std::env::current_exe()?;
    let mut delay = INITIAL_RESTART_DELAY;
    loop {
        let started = tokio::time::Instant::now();
        let child = std::process::Command::new(&executable)
            .arg(CONTROL_COMMAND)
            .arg(CONFIGURATION_ARGUMENT)
            .arg(configuration)
            .spawn()?;
        let control = i32::try_from(child.id())?;
        tracing::info!(process_id = control, "started the control process");
        while let Some(process_id) = ended.recv().await {
            if process_id == control {
                break;
            }
        }
        if started.elapsed() >= HEALTHY_RUN {
            delay = INITIAL_RESTART_DELAY;
        }
        tracing::error!(?delay, "the control process ended; starting it again");
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(MAXIMUM_RESTART_DELAY);
    }
}

pub async fn run_as_process_one() -> anyhow::Result<()> {
    let (configuration, modules) = boot::early_boot().await?;
    let watchdog = Arc::new(KernelWatchdog::new(
        boot::boot_modules_directory(),
        configuration.fencing.terms().watchdog_interval(),
    ));
    answer_control_requests(watchdog)?;
    mark_modules_ready(modules);
    let (ended, endings) = mpsc::unbounded_channel();
    reap(ended);
    supervise_control(Path::new(boot::CONFIGURATION_FILE), endings).await
}
