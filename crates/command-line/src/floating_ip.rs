use std::net::Ipv4Addr;

use amafu_api::ApiClient;
use amafu_domain::kinds::external_network::{ExternalNetwork, ExternalNetworkSpecification};
use amafu_domain::kinds::floating_ip::{FloatingIp, FloatingIpSpecification, FloatingIpTarget};
use amafu_domain::{Ipv4Network, Object, ObjectName};
use clap::Subcommand;

use crate::output::OutputFormat;
use crate::resources::{CommonCommand, Tabulated, create, kebab_name, or_dash, print_objects, run_common};

#[derive(Subcommand)]
pub enum ExternalNetworkCommand {
    #[command(about = "Declare a range of addresses on the underlay for floating IPs")]
    Create {
        name: ObjectName,
        #[arg(long, value_name = "CIDR")]
        subnet: Ipv4Network,
        #[arg(long, value_name = "ADDRESS")]
        first: Ipv4Addr,
        #[arg(long, value_name = "ADDRESS")]
        last: Ipv4Addr,
    },
    #[command(flatten)]
    Common(CommonCommand),
}

#[derive(Subcommand)]
pub enum FloatingIpCommand {
    #[command(about = "Reserve a floating IP, optionally pointing at a VM")]
    Create {
        name: ObjectName,
        #[arg(long = "external-network", value_name = "NETWORK")]
        external_network: ObjectName,
        #[arg(long, value_name = "ADDRESS", help = "Ask for this address instead of the next free one")]
        address: Option<Ipv4Addr>,
        #[arg(long, value_name = "VM")]
        vm: Option<ObjectName>,
        #[arg(long, default_value_t = 0, help = "The VM interface the address reaches")]
        interface: u8,
    },
    #[command(about = "Point a floating IP at another VM, or at none")]
    Move {
        name: String,
        #[arg(long, value_name = "VM")]
        vm: Option<ObjectName>,
        #[arg(long, default_value_t = 0)]
        interface: u8,
    },
    #[command(flatten)]
    Common(CommonCommand),
}

impl Tabulated for ExternalNetwork {
    const HEADINGS: &'static [&'static str] = &["NAME", "SUBNET", "RANGE"];

    fn row(network: &Object<Self>) -> Vec<String> {
        let range = &network.specification;
        vec![
            network.metadata.name.to_string(),
            range.subnet.to_string(),
            format!("{}-{}", range.first_address, range.last_address),
        ]
    }
}

impl Tabulated for FloatingIp {
    const HEADINGS: &'static [&'static str] = &["NAME", "PHASE", "ADDRESS", "VM", "NODE"];

    fn row(floating_ip: &Object<Self>) -> Vec<String> {
        vec![
            floating_ip.metadata.name.to_string(),
            kebab_name(floating_ip.status.phase),
            or_dash(floating_ip.status.address.map(|address| address.to_string())),
            or_dash(floating_ip.specification.target.as_ref().map(|target| target.vm.to_string())),
            or_dash(floating_ip.status.binding.as_ref().map(|binding| binding.node.to_string())),
        ]
    }
}

fn target(vm: Option<ObjectName>, interface: u8) -> Option<FloatingIpTarget> {
    vm.map(|vm| FloatingIpTarget { vm, interface })
}

pub async fn run_external(
    client: &ApiClient,
    command: ExternalNetworkCommand,
    format: OutputFormat,
) -> anyhow::Result<()> {
    match command {
        ExternalNetworkCommand::Create { name, subnet, first, last } => {
            let specification =
                ExternalNetworkSpecification { subnet, first_address: first, last_address: last };
            create::<ExternalNetwork>(client, &name, &specification, format).await
        }
        ExternalNetworkCommand::Common(command) => {
            run_common::<ExternalNetwork>(client, command, format).await
        }
    }
}

pub async fn run(client: &ApiClient, command: FloatingIpCommand, format: OutputFormat) -> anyhow::Result<()> {
    match command {
        FloatingIpCommand::Create { name, external_network, address, vm, interface } => {
            let specification =
                FloatingIpSpecification { external_network, address, target: target(vm, interface) };
            create::<FloatingIp>(client, &name, &specification, format).await
        }
        FloatingIpCommand::Move { name, vm, interface } => {
            let current = client.get::<FloatingIp>(&name).await?;
            let specification =
                FloatingIpSpecification { target: target(vm, interface), ..current.specification };
            let moved = client.update::<FloatingIp>(&name, &specification, None).await?;
            print_objects(&[moved], format)
        }
        FloatingIpCommand::Common(command) => run_common::<FloatingIp>(client, command, format).await,
    }
}
