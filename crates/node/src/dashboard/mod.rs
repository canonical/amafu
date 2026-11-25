mod process;
mod supervisor;

use std::net::SocketAddrV4;
use std::sync::Arc;

use amafu_node_configuration::NodeConfiguration;
use tokio::sync::watch;
use tokio::task::JoinSet;

use crate::cluster_address::settings::ClusterSettings;
use crate::roles::supervise;

pub use process::{TlsPair, run_dashboard_process};
pub use supervisor::DashboardSupervisor;

pub const DASHBOARD_PORT: u16 = 443;

fn upstreams(configuration: &NodeConfiguration) -> Vec<String> {
    let own_address = configuration.state.listen.ip();
    let mut core_members: Vec<SocketAddrV4> = configuration.platform_api();
    core_members.sort_by_key(|member| std::net::IpAddr::V4(*member.ip()) != own_address);
    core_members.iter().map(|member| format!("https://{member}")).collect()
}

pub fn start(
    roles: &mut JoinSet<()>,
    configuration: &NodeConfiguration,
    settings: watch::Receiver<ClusterSettings>,
) -> anyhow::Result<()> {
    let Some(identity) = &configuration.identity else {
        tracing::info!("this node has no cluster identity, so it does not serve the dashboard");
        return Ok(());
    };
    let supervisor = Arc::new(DashboardSupervisor {
        executable: std::env::current_exe()?,
        node_identity: TlsPair {
            certificate_pem: configuration.api.certificate.clone(),
            private_key_pem: configuration.api.private_key.clone(),
        },
        certificate_authority_pem: identity.certificate_authority.clone(),
        upstreams: upstreams(configuration),
        runtime_directory: configuration.directories.runtime.clone(),
        settings,
    });
    roles.spawn(supervise("dashboard", move || {
        let supervisor = Arc::clone(&supervisor);
        async move { supervisor.serve().await }
    }));
    Ok(())
}
