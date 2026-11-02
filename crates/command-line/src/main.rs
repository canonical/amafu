mod connection;
mod image;
mod node;
mod output;
mod resources;
mod storage_backend;
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
    #[command(subcommand, about = "Register disk and container images")]
    Image(image::ImageCommand),
    #[command(subcommand, about = "Create and manage volumes")]
    Volume(volume::VolumeCommand),
    #[command(subcommand, about = "Declare where volumes are kept")]
    StorageBackend(storage_backend::StorageBackendCommand),
    #[command(subcommand, about = "Inspect and manage nodes")]
    Node(node::NodeCommand),
}

async fn run(arguments: Arguments) -> anyhow::Result<()> {
    let client = arguments.connection.client()?;
    match arguments.command {
        Command::Vm(command) => vm::run(&client, command, arguments.output).await,
        Command::Image(command) => image::run(&client, command, arguments.output).await,
        Command::Volume(command) => volume::run(&client, command, arguments.output).await,
        Command::StorageBackend(command) => storage_backend::run(&client, command, arguments.output).await,
        Command::Node(command) => node::run(&client, command, arguments.output).await,
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
