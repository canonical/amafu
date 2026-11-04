mod boot;
mod clock;
mod diagnostics;
mod roles;
mod watchdog;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};

use amafu_node::configuration::NodeConfiguration;

const REBOOT_DELAY: Duration = Duration::from_secs(30);

#[derive(Parser)]
#[command(name = "amafu-node", about = "The Amafu node: PID 1, every role, and the per-VM processes")]
struct Arguments {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    Run {
        #[arg(long)]
        configuration: PathBuf,
    },
    VmProcess {
        #[arg(long)]
        directory: PathBuf,
    },
}

fn initialize_logging() {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
}

fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread().enable_all().build()
}

async fn run_as_process_one() -> anyhow::Result<()> {
    let (configuration, modules) = boot::early_boot().await?;
    tokio::spawn(async {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        loop {
            interval.tick().await;
            for reaped in amafu_system::reap_orphans() {
                tracing::info!(
                    process_id = reaped.process_id,
                    exit_code = reaped.exit_code,
                    signal = reaped.signal,
                    "a child process ended"
                );
            }
        }
    });
    let watchdog = Arc::new(watchdog::KernelWatchdog::new(
        boot::boot_modules_directory(),
        configuration.fencing.terms().watchdog_interval(),
    ));
    roles::run(configuration, Some(roles::BootServices { modules, watchdog })).await
}

fn fail_as_process_one(error: &anyhow::Error) -> ExitCode {
    tracing::error!(error = format!("{error:#}"), ?REBOOT_DELAY, "the node cannot start; rebooting");
    std::thread::sleep(REBOOT_DELAY);
    if let Err(reboot_error) = amafu_system::reboot() {
        tracing::error!(%reboot_error, "rebooting failed");
    }
    loop {
        std::thread::park();
    }
}

fn main() -> ExitCode {
    initialize_logging();
    let arguments = Arguments::parse();
    let outcome = match arguments.command {
        Some(Command::VmProcess { directory }) => {
            amafu_hypervisor::run_vm_process(&directory).map_err(anyhow::Error::from)
        }
        Some(Command::Run { configuration }) => runtime().map_err(anyhow::Error::from).and_then(|runtime| {
            runtime.block_on(async { roles::run(NodeConfiguration::load(&configuration)?, None).await })
        }),
        None if amafu_system::is_process_one() => match runtime() {
            Ok(runtime) => match runtime.block_on(run_as_process_one()) {
                Ok(()) => Ok(()),
                Err(error) => return fail_as_process_one(&error),
            },
            Err(error) => return fail_as_process_one(&error.into()),
        },
        None => Err(anyhow::anyhow!("run as PID 1, or give a command; see --help")),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(error = format!("{error:#}"), "amafu-node failed");
            ExitCode::FAILURE
        }
    }
}
