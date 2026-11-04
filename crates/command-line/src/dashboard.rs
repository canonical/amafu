use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use amafu_api::{HttpTransport, Transport};
use amafu_dashboard::{DashboardState, SessionKey, dashboard_router};
use amafu_domain::Sha256Digest;
use amafu_installer::configuration::{ClusterCredentials, api_endpoint};
use amafu_installer::maas::MaasClient;
use amafu_installer::management::{ManagementPlan, enable_dashboard};
use amafu_installer::{InstallDescription, InstallDirectory};
use amafu_interfaces::{MachineName, MachineProvisioner};
use anyhow::Context;
use clap::Subcommand;

const SECONDS_PER_HOUR: u64 = 3600;

#[derive(Subcommand)]
pub enum DashboardCommand {
    #[command(about = "Serve the dashboard, forwarding every call with the signed-in user's own token")]
    Serve {
        #[arg(long, default_value = "0.0.0.0:443")]
        listen: std::net::SocketAddr,
        #[arg(long = "core-endpoint", required = true, help = "A core member's API, repeated for each")]
        core_endpoints: Vec<String>,
        #[arg(long, help = "The key every dashboard instance seals sessions with")]
        session_key_file: PathBuf,
        #[arg(long)]
        certificate: PathBuf,
        #[arg(long)]
        private_key: PathBuf,
        #[arg(long, default_value_t = 12)]
        session_hours: u64,
    },
    #[command(about = "Create the key that dashboard instances share to seal sessions")]
    CreateSessionKey {
        #[arg(long)]
        file: PathBuf,
    },
    #[command(about = "Deploy the dashboard on management machines from MAAS, behind one virtual address")]
    Enable {
        #[arg(long, help = "The install description the cluster was installed with")]
        description: PathBuf,
        #[arg(long, help = "The installer's state directory, which holds the cluster authority")]
        state_directory: PathBuf,
        #[arg(
            long = "machine",
            required = true,
            help = "A MAAS machine to deploy the dashboard on, repeated"
        )]
        machines: Vec<String>,
        #[arg(long, help = "The shared address with its prefix length, for example 10.0.0.50/24")]
        virtual_address: String,
        #[arg(long, default_value = "eth0")]
        interface: String,
        #[arg(long, help = "Where the management machines download this command")]
        command_line_url: String,
        #[arg(long)]
        command_line_digest: Sha256Digest,
        #[arg(long)]
        session_key_file: PathBuf,
    },
}

fn read(path: &PathBuf) -> anyhow::Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

fn write_private(path: &PathBuf, contents: &str) -> anyhow::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    std::io::Write::write_all(
        &mut options.open(path).with_context(|| format!("creating {}", path.display()))?,
        contents.as_bytes(),
    )?;
    Ok(())
}

pub async fn run(command: DashboardCommand, certificate_authority: Option<&PathBuf>) -> anyhow::Result<()> {
    match command {
        DashboardCommand::Serve {
            listen,
            core_endpoints,
            session_key_file,
            certificate,
            private_key,
            session_hours,
        } => {
            let authority = certificate_authority.map(read).transpose()?;
            let upstreams = core_endpoints
                .iter()
                .map(|endpoint| {
                    HttpTransport::new(endpoint, authority.as_deref().map(str::as_bytes))
                        .map(|transport| Arc::new(transport) as Arc<dyn Transport>)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let key = SessionKey::from_hexadecimal(&read(&session_key_file)?)?;
            let state =
                DashboardState::new(key, upstreams, Duration::from_secs(session_hours * SECONDS_PER_HOUR));
            let tls = amafu_api::server::tls_configuration(
                read(&certificate)?.as_bytes(),
                read(&private_key)?.as_bytes(),
            )?;
            let listener = tokio::net::TcpListener::bind(listen).await?;
            println!("the dashboard is serving on {listen}");
            Err(amafu_api::server::serve(listener, Some(tls), dashboard_router(state)).await.into())
        }
        DashboardCommand::CreateSessionKey { file } => {
            write_private(&file, &SessionKey::generate_hexadecimal()?)
        }
        DashboardCommand::Enable {
            description,
            state_directory,
            machines,
            virtual_address,
            interface,
            command_line_url,
            command_line_digest,
            session_key_file,
        } => {
            let description = InstallDescription::load(&description)?;
            let directory = InstallDirectory::new(&state_directory);
            let credentials =
                ClusterCredentials::load_or_create(&directory.credentials(), &description.cluster.name)?;
            let maas = &description.maas;
            let provisioner: Arc<dyn MachineProvisioner> =
                Arc::new(MaasClient::new(&maas.url, &maas.api_key, maas.certificate_authority.as_deref())?);
            let plan = ManagementPlan {
                machines: machines.into_iter().map(MachineName).collect(),
                virtual_address,
                interface,
                core_endpoints: description.core_members().map(api_endpoint).collect(),
                command_line_url,
                command_line_digest,
                session_key: read(&session_key_file)?.trim().to_owned(),
            };
            enable_dashboard(&plan, &credentials.authority, provisioner).await?;
            println!("the dashboard answers on {}", plan.virtual_address);
            Ok(())
        }
    }
}
