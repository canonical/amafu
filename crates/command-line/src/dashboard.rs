use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use amafu_api::{HttpTransport, Transport};
use amafu_dashboard::{DashboardState, SessionKey, dashboard_router};
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
            crate::output::emit_line(format!("the dashboard is serving on {listen}"))?;
            Err(amafu_api::server::serve(listener, Some(Arc::new(tls)), dashboard_router(state)).await.into())
        }
        DashboardCommand::CreateSessionKey { file } => {
            write_private(&file, &SessionKey::generate_hexadecimal()?)
        }
    }
}
