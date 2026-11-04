use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use amafu_installer::maas::MaasClient;
use amafu_installer::{InstallDescription, InstallDirectory, InstallSession};
use amafu_interfaces::{Clock, MachineProvisioner};
use anyhow::Context;
use clap::Args;
use futures::future::BoxFuture;
use time::OffsetDateTime;

#[derive(Args)]
pub struct InstallArguments {
    #[arg(long, help = "The install description")]
    description: PathBuf,
    #[arg(long, help = "Where the installer keeps its journal and the cluster credentials")]
    state_directory: PathBuf,
}

struct WallClock;

impl Clock for WallClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(duration))
    }
}

pub async fn run(arguments: InstallArguments) -> anyhow::Result<()> {
    let description = InstallDescription::load(&arguments.description)
        .with_context(|| format!("reading {}", arguments.description.display()))?;
    std::fs::create_dir_all(&arguments.state_directory)?;
    let maas = &description.maas;
    let provisioner = MaasClient::new(&maas.url, &maas.api_key, maas.certificate_authority.as_deref())?;
    let directory = InstallDirectory::new(&arguments.state_directory);
    let session = InstallSession::open(
        description,
        &directory,
        Arc::new(provisioner) as Arc<dyn MachineProvisioner>,
        None,
        Arc::new(WallClock),
    )
    .await?;
    session.run_to_completion().await?;
    println!("the cluster is installed; its credentials are in {}", directory.credentials().display());
    Ok(())
}
