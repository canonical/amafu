use amafu_api::ApiClient;
use amafu_domain::kinds::network::{Network, NetworkSpecification};
use amafu_domain::{Ipv4Network, Object, ObjectName};
use clap::Subcommand;

use crate::output::OutputFormat;
use crate::resources::{CommonCommand, Tabulated, create, kebab_name, or_dash, print_objects, run_common};

#[derive(Subcommand)]
pub enum NetworkCommand {
    #[command(about = "Create a private network")]
    Create {
        name: ObjectName,
        #[arg(long, value_name = "CIDR")]
        subnet: Ipv4Network,
        #[arg(long, help = "Keep VMs on this network from reaching outside it")]
        no_outbound_access: bool,
        #[arg(long, help = "Let VMs on this network reach the Amafu API, as Kubernetes and Juju need")]
        allow_platform_api: bool,
    },
    #[command(name = "allow-platform-api", about = "Let VMs on a network reach the Amafu API")]
    AllowPlatformApi { name: String },
    #[command(name = "block-platform-api", about = "Keep VMs on a network from reaching the Amafu API")]
    BlockPlatformApi { name: String },
    #[command(flatten)]
    Common(CommonCommand),
}

impl Tabulated for Network {
    const HEADINGS: &'static [&'static str] =
        &["NAME", "PHASE", "SUBNET", "GATEWAY", "OUTBOUND", "PLATFORM API"];

    fn row(network: &Object<Self>) -> Vec<String> {
        vec![
            network.metadata.name.to_string(),
            kebab_name(network.status.phase),
            network.specification.subnet.to_string(),
            or_dash(network.status.gateway.map(|gateway| gateway.to_string())),
            allowance(network.specification.is_outbound_access_allowed),
            allowance(network.specification.is_platform_api_allowed),
        ]
    }
}

fn allowance(is_allowed: bool) -> String {
    String::from(if is_allowed { "allowed" } else { "blocked" })
}

async fn set_platform_api(
    client: &ApiClient,
    name: &str,
    is_platform_api_allowed: bool,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let network = client.get::<Network>(name).await?;
    let specification = NetworkSpecification { is_platform_api_allowed, ..network.specification };
    let updated = client.update::<Network>(name, &specification, Some(network.metadata.generation)).await?;
    print_objects(&[updated], format)
}

pub async fn run(client: &ApiClient, command: NetworkCommand, format: OutputFormat) -> anyhow::Result<()> {
    match command {
        NetworkCommand::Create { name, subnet, no_outbound_access, allow_platform_api } => {
            let specification = NetworkSpecification {
                subnet,
                is_outbound_access_allowed: !no_outbound_access,
                is_platform_api_allowed: allow_platform_api,
            };
            create::<Network>(client, &name, &specification, format).await
        }
        NetworkCommand::AllowPlatformApi { name } => set_platform_api(client, &name, true, format).await,
        NetworkCommand::BlockPlatformApi { name } => set_platform_api(client, &name, false, format).await,
        NetworkCommand::Common(command) => run_common::<Network>(client, command, format).await,
    }
}
