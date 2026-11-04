mod connection;
mod container;
mod dashboard;
mod floating_ip;
mod image;
mod install;
mod network;
mod node;
mod operation;
mod output;
mod release;
mod resources;
mod security_group;
mod storage_backend;
mod upgrade;
mod vm;
mod volume;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::connection::ConnectionArguments;
use crate::output::OutputFormat;

#[derive(Parser)]
#[command(name = "amafu", about = "Manage an Amafu cluster", version)]
struct Arguments {
    #[command(flatten)]
    connection: ConnectionArguments,
    #[arg(long, global = true, value_enum, default_value_t = OutputFormat::Table)]
    output: OutputFormat,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(subcommand, about = "Create and manage virtual machines")]
    Vm(vm::VmCommand),
    #[command(subcommand, about = "Run container images in small VMs")]
    Container(container::ContainerCommand),
    #[command(subcommand, about = "Register disk and container images")]
    Image(image::ImageCommand),
    #[command(subcommand, about = "Create and manage volumes")]
    Volume(volume::VolumeCommand),
    #[command(subcommand, about = "Create and manage private networks")]
    Network(network::NetworkCommand),
    #[command(subcommand, about = "Declare address ranges for floating IPs")]
    ExternalNetwork(floating_ip::ExternalNetworkCommand),
    #[command(subcommand, about = "Reach VMs from outside through addresses that can move")]
    FloatingIp(floating_ip::FloatingIpCommand),
    #[command(subcommand, about = "Control traffic to and from VM interfaces")]
    SecurityGroup(security_group::SecurityGroupCommand),
    #[command(subcommand, about = "Declare where volumes are kept")]
    StorageBackend(storage_backend::StorageBackendCommand),
    #[command(subcommand, about = "Inspect and manage nodes")]
    Node(node::NodeCommand),
    #[command(
        subcommand,
        about = "Follow, pause and cancel migrations, evacuations, recoveries and upgrades"
    )]
    Operation(operation::OperationCommand),
    #[command(subcommand, about = "Upgrade the cluster to a new image, roll back or finalize")]
    Upgrade(upgrade::UpgradeCommand),
    #[command(about = "Deploy a new cluster from MAAS")]
    Install(install::InstallArguments),
    #[command(subcommand, about = "Create signing keys and sign node images")]
    Release(release::ReleaseCommand),
    #[command(subcommand, about = "Serve or deploy the web dashboard")]
    Dashboard(dashboard::DashboardCommand),
}

async fn run(arguments: Arguments) -> anyhow::Result<()> {
    let command = match arguments.command {
        Command::Install(install) => return install::run(install).await,
        Command::Release(command) => return release::run(command),
        Command::Dashboard(command) => {
            return dashboard::run(command, arguments.connection.certificate_authority()).await;
        }
        command => command,
    };
    let client = arguments.connection.client()?;
    match command {
        Command::Vm(command) => vm::run(&client, command, arguments.output).await,
        Command::Container(command) => container::run(&client, command, arguments.output).await,
        Command::Image(command) => image::run(&client, command, arguments.output).await,
        Command::Volume(command) => volume::run(&client, command, arguments.output).await,
        Command::Network(command) => network::run(&client, command, arguments.output).await,
        Command::ExternalNetwork(command) => {
            floating_ip::run_external(&client, command, arguments.output).await
        }
        Command::FloatingIp(command) => floating_ip::run(&client, command, arguments.output).await,
        Command::SecurityGroup(command) => security_group::run(&client, command, arguments.output).await,
        Command::StorageBackend(command) => storage_backend::run(&client, command, arguments.output).await,
        Command::Node(command) => node::run(&client, command, arguments.output).await,
        Command::Operation(command) => operation::run(&client, command, arguments.output).await,
        Command::Upgrade(command) => upgrade::run(&client, command, arguments.output).await,
        Command::Install(_) | Command::Release(_) | Command::Dashboard(_) => Ok(()),
    }
}

fn main() -> ExitCode {
    let arguments = Arguments::parse();
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(arguments)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}
