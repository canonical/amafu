use std::net::Ipv4Addr;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use amafu_hypervisor::control::{self, ControlChannel};
use amafu_hypervisor::sandbox::{ServiceLimits, create_service_cgroup, place_in_cgroup};
use anyhow::Context;
use rustix::process::{Pid, Signal, kill_process};
use tokio::process::{Child, Command};
use tokio::sync::watch;

use super::DASHBOARD_PORT;
use super::process::{DASHBOARD_COMMAND, DashboardHandOver, TlsPair};
use crate::cluster_address::settings::ClusterSettings;

pub const DASHBOARD_USER_ID: u32 = 64_100;
const PROCESS_ID_FILE: &str = "dashboard.pid";
const CGROUP_NAME: &str = "dashboard";
const LIMITS: ServiceLimits = ServiceLimits { cpu_count: 1, memory_mebibytes: 512, processes: 256 };

pub struct DashboardSupervisor {
    pub executable: PathBuf,
    pub node_identity: TlsPair,
    pub certificate_authority_pem: String,
    pub upstreams: Vec<String>,
    pub runtime_directory: PathBuf,
    pub settings: watch::Receiver<ClusterSettings>,
}

impl DashboardSupervisor {
    pub async fn serve(&self) -> String {
        let mut settings = self.settings.clone();
        let hand_over = loop {
            if let Some(hand_over) = self.hand_over(&settings.borrow_and_update()) {
                break hand_over;
            }
            if settings.changed().await.is_err() {
                return String::from("the cluster address settings are no longer kept");
            }
        };
        stop_the_previous_dashboard(&self.process_id_file());
        let mut child = match self.launch(&hand_over) {
            Ok(child) => child,
            Err(error) => return format!("starting the dashboard failed: {error:#}"),
        };
        tokio::select! {
            ended = child.wait() => format!("the dashboard process ended: {ended:?}"),
            () = self.until_the_hand_over_changes(&mut settings, &hand_over) => {
                let _ = child.kill().await;
                String::from("the dashboard's settings changed, so it restarts with them")
            }
        }
    }

    fn hand_over(&self, settings: &ClusterSettings) -> Option<DashboardHandOver> {
        if !settings.can_serve_the_dashboard() {
            return None;
        }
        let cluster_identity =
            settings.address.as_ref().zip(settings.certificate.as_ref()).map(|(address, certificate)| {
                let pair = TlsPair {
                    certificate_pem: certificate.certificate_pem.clone(),
                    private_key_pem: certificate.private_key_pem.clone(),
                };
                (address.address, pair)
            });
        Some(DashboardHandOver {
            node_identity: self.node_identity.clone(),
            cluster_identity,
            certificate_authority_pem: self.certificate_authority_pem.clone(),
            session_key_hexadecimal: settings.session_key.as_ref()?.key_hexadecimal.clone(),
            upstreams: self.upstreams.clone(),
        })
    }

    async fn until_the_hand_over_changes(
        &self,
        settings: &mut watch::Receiver<ClusterSettings>,
        current: &DashboardHandOver,
    ) {
        while settings.changed().await.is_ok() {
            if self.hand_over(&settings.borrow_and_update()).as_ref() != Some(current) {
                return;
            }
        }
        std::future::pending::<()>().await;
    }

    fn launch(&self, hand_over: &DashboardHandOver) -> anyhow::Result<Child> {
        let listener = std::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, DASHBOARD_PORT))
            .with_context(|| format!("listening on port {DASHBOARD_PORT}"))?;
        let cgroup = create_service_cgroup(CGROUP_NAME, LIMITS).context("creating the dashboard's cgroup")?;
        let channel = ControlChannel::new()?;
        let mut child = Command::new(&self.executable)
            .arg(DASHBOARD_COMMAND)
            .uid(DASHBOARD_USER_ID)
            .gid(DASHBOARD_USER_ID)
            .env_clear()
            .env("RUST_LOG", "info")
            .stdin(Stdio::from(channel.child))
            .kill_on_drop(true)
            .spawn()
            .context("starting the dashboard process")?;
        let process_id = child.id().context("the dashboard process ended at once")?;
        let handed_over = place_in_cgroup(&cgroup, process_id)
            .and_then(|()| control::send_message(&channel.parent, hand_over, &[listener.as_fd()]))
            .and_then(|()| std::fs::write(self.process_id_file(), process_id.to_string()));
        if let Err(error) = handed_over {
            let _ = child.start_kill();
            return Err(error).context("handing the dashboard its settings");
        }
        Ok(child)
    }

    fn process_id_file(&self) -> PathBuf {
        self.runtime_directory.join(PROCESS_ID_FILE)
    }
}

fn is_a_dashboard_process(process_id: i32) -> bool {
    std::fs::read(format!("/proc/{process_id}/cmdline")).is_ok_and(|command_line| {
        command_line.split(|byte| *byte == 0).any(|word| word == DASHBOARD_COMMAND.as_bytes())
    })
}

fn stop_the_previous_dashboard(process_id_file: &Path) {
    let previous =
        std::fs::read_to_string(process_id_file).ok().and_then(|text| text.trim().parse::<i32>().ok());
    let Some(process_id) = previous.filter(|process_id| is_a_dashboard_process(*process_id)) else {
        return;
    };
    if let Some(pid) = Pid::from_raw(process_id) {
        tracing::info!(process_id, "stopping the dashboard a previous control process started");
        let _ = kill_process(pid, Signal::KILL);
    }
}
