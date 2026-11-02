use amafu_api::ApiClient;
use amafu_domain::kinds::network::{Network, NetworkSpecification};
use amafu_domain::{Ipv4Network, Object, ObjectName};
use clap::Subcommand;

use crate::output::OutputFormat;
use crate::resources::{CommonCommand, Tabulated, create, kebab_name, or_dash, run_common};

#[derive(Subcommand)]
pub enum NetworkCommand {
    #[command(about = "Create a private network")]
    Create {
        name: ObjectName,
        #[arg(long, value_name = "CIDR")]
        subnet: Ipv4Network,
        #[arg(long, help = "Keep VMs on this network from reaching outside it")]
        no_outbound_access: bool,
    },
    #[command(flatten)]
    Common(CommonCommand),
}

impl Tabulated for Network {
    const HEADINGS: &'static [&'static str] = &["NAME", "PHASE", "SUBNET", "GATEWAY", "OUTBOUND"];

    fn row(network: &Object<Self>) -> Vec<String> {
        vec![
            network.metadata.name.to_string(),
            kebab_name(network.status.phase),
            network.specification.subnet.to_string(),
            or_dash(network.status.gateway.map(|gateway| gateway.to_string())),
            String::from(if network.specification.is_outbound_access_allowed {
                "allowed"
            } else {
                "blocked"
            }),
        ]
    }
}

pub async fn run(client: &ApiClient, command: NetworkCommand, format: OutputFormat) -> anyhow::Result<()> {
    match command {
        NetworkCommand::Create { name, subnet, no_outbound_access } => {
            let specification =
                NetworkSpecification { subnet, is_outbound_access_allowed: !no_outbound_access };
            create::<Network>(client, &name, &specification, format).await
        }
        NetworkCommand::Common(command) => run_common::<Network>(client, command, format).await,
    }
}
