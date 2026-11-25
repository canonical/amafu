use std::path::PathBuf;

use amafu_api::ApiClient;
use amafu_domain::kinds::cluster_address::{
    CLUSTER_ADDRESS_NAME, ClusterAddress, ClusterAddressCertificateSpecification, ClusterAddressSpecification,
};
use amafu_installer::InstallDirectory;
use amafu_installer::cluster_address::{configure_cluster_address, issue_certificate};
use amafu_installer::configuration::ClusterCredentials;
use anyhow::Context;
use clap::{Args, Subcommand};

use crate::output::{OutputFormat, print_structured};

#[derive(Subcommand)]
pub enum ClusterAddressCommand {
    #[command(about = "Show the address the dashboard and the API answer on")]
    Show,
    #[command(about = "Give the cluster an address that its dashboard and API answer on")]
    Set(SetArguments),
}

#[derive(Args)]
pub struct SetArguments {
    #[arg(help = "The address with the prefix length of the nodes' network, for example 10.20.0.50/24")]
    address: ClusterAddressSpecification,
    #[arg(long, help = "The interface to hold it on; by default the one already on that network")]
    interface: Option<String>,
    #[arg(
        long,
        help = "The installer's state directory, whose cluster authority issues the address's certificate",
        required_unless_present = "certificate",
        conflicts_with = "certificate"
    )]
    state_directory: Option<PathBuf>,
    #[arg(
        long,
        help = "A certificate chain for the address, issued by an authority of your choice",
        requires = "private_key"
    )]
    certificate: Option<PathBuf>,
    #[arg(long, help = "The private key of that certificate", requires = "certificate")]
    private_key: Option<PathBuf>,
}

fn read(path: &PathBuf) -> anyhow::Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

impl SetArguments {
    fn certificate_for(
        &self,
        address: &ClusterAddressSpecification,
    ) -> anyhow::Result<ClusterAddressCertificateSpecification> {
        if let (Some(certificate), Some(private_key)) = (&self.certificate, &self.private_key) {
            return Ok(ClusterAddressCertificateSpecification {
                certificate_pem: read(certificate)?,
                private_key_pem: read(private_key)?,
            });
        }
        let state_directory =
            self.state_directory.as_ref().context("a certificate or a state directory is needed")?;
        let credentials = ClusterCredentials::load(&InstallDirectory::new(state_directory).credentials())
            .context("reading the installer's cluster credentials")?;
        Ok(issue_certificate(&credentials.authority, address)?)
    }
}

pub async fn run(
    client: &ApiClient,
    command: ClusterAddressCommand,
    format: OutputFormat,
) -> anyhow::Result<()> {
    match command {
        ClusterAddressCommand::Show => {
            print_structured(&client.get::<ClusterAddress>(CLUSTER_ADDRESS_NAME).await?, format)
        }
        ClusterAddressCommand::Set(arguments) => {
            let address = ClusterAddressSpecification {
                interface: arguments.interface.clone(),
                ..arguments.address.clone()
            };
            let certificate = arguments.certificate_for(&address)?;
            configure_cluster_address(client, &address, &certificate).await?;
            crate::output::emit_line(format!(
                "the dashboard will answer at https://{} and the API at https://{}:7443",
                address.address, address.address
            ))?;
            Ok(())
        }
    }
}
