use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use amafu_api::diagnostics::{
    ATTACHMENTS_PATH, ContractOutcome, HYPERVISOR_CONTRACT_PATH, INSTANCES_PATH, NETWORK_PATH, NodeInstance,
    SANDBOX_PATH,
};
use amafu_lxd_lab::{LabSettings, VirtualLab, down, load_state, up};
use amafu_scenario::{Level, ScenarioDefinition};
use amafu_scenarios::recovery::{ForcedStop, soak_forced_stops};
use amafu_simulated_world::{SimulationSettings, run_scenario};
use anyhow::{Context, bail};
use clap::{Parser, Subcommand};

const NODE_BOOT_BUDGET: Duration = Duration::from_secs(20);

#[derive(Parser)]
#[command(name = "cargo rig", about = "Run Amafu scenarios in the simulated world and the virtual lab")]
struct Arguments {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Run scenarios in the simulated world over many seeds")]
    Simulate {
        #[arg(long, default_value_t = 50)]
        seeds: u64,
        #[arg(long, default_value_t = 0)]
        first_seed: u64,
        #[arg(long)]
        scenario: Option<String>,
        #[arg(long, help = "Crash roles at random points")]
        crash_roles: bool,
    },
    #[command(about = "Replay one seed of the simulated world")]
    Replay {
        seed: u64,
        #[arg(long)]
        scenario: Option<String>,
        #[arg(long)]
        crash_roles: bool,
    },
    #[command(subcommand, about = "Manage the virtual lab on LXD")]
    Lab(LabCommand),
    #[command(about = "List the scenarios and the levels they run at")]
    Scenarios,
    #[command(
        about = "Run real core members on this machine against many simulated nodes and check the budgets"
    )]
    Scale {
        #[arg(long, default_value_t = 1_000)]
        nodes: usize,
        #[arg(long, default_value_t = 2_000)]
        vms: usize,
        #[arg(long, default_value_t = 2_000)]
        read_rate: u32,
        #[arg(long, default_value_t = 30)]
        read_seconds: u64,
        #[arg(long, help = "Keep the core members' state on tmpfs, to tell disk stalls from Amafu's own")]
        state_in_memory: bool,
    },
}

fn scale(
    nodes: usize,
    vms: usize,
    read_rate: u32,
    read_seconds: u64,
    state_in_memory: bool,
) -> anyhow::Result<()> {
    let repository = repository()?;
    let node_binary = repository.join("target/release/amafu-node");
    if !node_binary.is_file() {
        bail!("build the node first: cargo build --release -p amafu-node");
    }
    let settings = amafu_scale::ScaleSettings {
        node_binary,
        work: amafu_scale::cores::work_directory(&repository),
        state: if state_in_memory {
            std::path::PathBuf::from("/dev/shm/amafu-scale")
        } else {
            amafu_scale::cores::work_directory(&repository).join("state")
        },
        core_members: 3,
        nodes,
        vms,
        read_rate,
        read_duration: Duration::from_secs(read_seconds),
    };
    let budgets = amafu_scale::ScaleBudgets { read_rate, ..amafu_scale::ScaleBudgets::default() };
    let report = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(amafu_scale::run(&settings, budgets))?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !report.violations.is_empty() {
        bail!("the scale scenario missed its budgets: {}", report.violations.join("; "));
    }
    Ok(())
}

#[derive(Subcommand)]
enum LabCommand {
    #[command(about = "Create the virtual lab from image/output/test")]
    Up {
        #[arg(long, default_value_t = 4)]
        nodes: u8,
    },
    #[command(about = "Delete the virtual lab and its LXD project")]
    Down,
    #[command(about = "Run scenarios against the virtual lab")]
    Run {
        #[arg(long)]
        scenario: Option<String>,
    },
    #[command(about = "Power nodes off without warning, again and again, and check their VMs recover")]
    Soak {
        #[arg(long, default_value_t = 50)]
        forced_stops: u32,
    },
    #[command(about = "Run the hypervisor contract against the real adapter on every node")]
    Contract,
    #[command(subcommand, about = "Manage the MAAS lab, whose nodes are installed as customers install them")]
    Maas(MaasCommand),
    #[command(about = "Show the VM processes running on every node, with their console output")]
    Instances,
    #[command(about = "Show what every node reports about one of its subsystems")]
    Inspect {
        #[arg(value_enum)]
        subsystem: Subsystem,
    },
}

#[derive(Subcommand)]
enum MaasCommand {
    #[command(about = "Create a MAAS VM and empty machines it commissions")]
    Up {
        #[arg(long, default_value_t = 3)]
        nodes: u8,
    },
    #[command(about = "Install a cluster on the commissioned machines with amafu install")]
    Install,
    #[command(about = "Delete the MAAS lab")]
    Down,
}

async fn maas_lab(repository: &std::path::Path, command: MaasCommand) -> anyhow::Result<()> {
    let settings = LabSettings::default();
    match command {
        MaasCommand::Up { nodes } => {
            let state = amafu_lxd_lab::maas_lab::up(repository, &settings, nodes).await?;
            println!("the MAAS lab commissioned {} machines", state.machines.len());
        }
        MaasCommand::Install => {
            let took = amafu_lxd_lab::maas_lab::install(&settings).await?;
            println!("amafu install deployed the MAAS lab in {} seconds", took.as_secs());
        }
        MaasCommand::Down => amafu_lxd_lab::maas_lab::down(repository, &settings).await?,
    }
    Ok(())
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Subsystem {
    Network,
    Attachments,
    Sandbox,
}

impl Subsystem {
    fn path(self) -> &'static str {
        match self {
            Self::Network => NETWORK_PATH,
            Self::Attachments => ATTACHMENTS_PATH,
            Self::Sandbox => SANDBOX_PATH,
        }
    }
}

fn repository() -> anyhow::Result<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.ancestors().nth(2).map(PathBuf::from).context("the rig is not inside the repository")
}

fn selected(level: Level, name: Option<&str>) -> anyhow::Result<Vec<ScenarioDefinition>> {
    let scenarios: Vec<ScenarioDefinition> = amafu_scenarios::all()
        .into_iter()
        .filter(|definition| definition.runs_at(level))
        .filter(|definition| name.is_none_or(|name| definition.name.contains(name)))
        .collect();
    if scenarios.is_empty() {
        bail!("no scenario matches at the {level} level");
    }
    Ok(scenarios)
}

fn simulate(seeds: std::ops::Range<u64>, name: Option<&str>, crash_roles: bool) -> anyhow::Result<()> {
    let mut failures = 0;
    for definition in selected(Level::Simulated, name)? {
        let mut passed = 0;
        for seed in seeds.clone() {
            let settings = SimulationSettings::with_seed(seed);
            let settings = if crash_roles { settings.crashing_roles() } else { settings };
            match run_scenario(&definition, settings) {
                Ok(_) => passed += 1,
                Err(failure) => {
                    failures += 1;
                    eprintln!("FAILED {}\n{failure}", definition.name);
                }
            }
        }
        println!("{passed}/{} seeds passed: {}", seeds.end - seeds.start, definition.name);
    }
    if failures > 0 {
        bail!("{failures} runs failed");
    }
    Ok(())
}

async fn soak(repository: &std::path::Path, forced_stops: u32) -> anyhow::Result<()> {
    let lab = VirtualLab::connect(load_state(repository)?)?;
    let report = |outcome: &Result<ForcedStop, amafu_scenario::ScenarioError>| match outcome {
        Ok(stop) => println!(
            "round {}: {} stopped, its VMs ran elsewhere after {:.1} s",
            stop.round,
            stop.node,
            stop.recovered_after.as_secs_f64()
        ),
        Err(error) => println!("FAILED: {error}"),
    };
    let failures = soak_forced_stops(&lab, forced_stops, &report).await?;
    println!("{} of {forced_stops} forced stops recovered", forced_stops - failures);
    if failures > 0 {
        bail!("{failures} forced stops did not recover in time");
    }
    Ok(())
}

async fn run_in_lab(repository: &std::path::Path, scenario: Option<&str>) -> anyhow::Result<()> {
    let lab = VirtualLab::connect(load_state(repository)?)?;
    let mut failures = 0;
    for definition in selected(Level::VirtualLab, scenario)? {
        match (definition.run)(&lab).await {
            Ok(()) => println!("passed: {}", definition.name),
            Err(error) => {
                failures += 1;
                println!("FAILED: {}: {error}", definition.name);
            }
        }
    }
    if failures > 0 {
        bail!("{failures} scenarios failed in the virtual lab");
    }
    Ok(())
}

async fn lab(command: LabCommand) -> anyhow::Result<()> {
    let repository = repository()?;
    match command {
        LabCommand::Up { nodes } => {
            let (state, boots) = up(&repository, LabSettings::default(), nodes).await?;
            for boot in &boots {
                println!("{}", serde_json::to_string_pretty(boot)?);
                let within_budget = boot.seconds_from_start_to_serving <= NODE_BOOT_BUDGET.as_secs_f64();
                println!(
                    "{}: serving {:.1} s after start (budget {} s: {}), Secure Boot {}, TPM {}, KVM {}",
                    boot.node,
                    boot.seconds_from_start_to_serving,
                    NODE_BOOT_BUDGET.as_secs(),
                    if within_budget { "met" } else { "MISSED" },
                    boot.facts.is_secure_boot_enabled,
                    boot.facts.is_tpm_present,
                    boot.facts.is_kvm_available,
                );
            }
            println!("lab is up in LXD project {}", state.settings.project);
            Ok(())
        }
        LabCommand::Down => down(&repository, &LabSettings::default()).await,
        LabCommand::Soak { forced_stops } => soak(&repository, forced_stops).await,
        LabCommand::Run { scenario } => run_in_lab(&repository, scenario.as_deref()).await,
        LabCommand::Instances => {
            let state = load_state(&repository)?;
            let http = reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?;
            for node in &state.topology.nodes {
                let url = format!("{}{INSTANCES_PATH}", node.diagnostics_endpoint(&state.settings));
                let instances: Vec<NodeInstance> = http.get(url).send().await?.json().await?;
                for instance in instances {
                    println!(
                        "{} {} {:?} workload {:?} disks {:?}\n--- console\n{}\n--- VM process\n{}",
                        node.name(),
                        instance.vm_id,
                        instance.state,
                        instance.workload,
                        instance.disks,
                        instance.console_tail.trim_end(),
                        instance.process_log_tail.trim_end()
                    );
                }
            }
            Ok(())
        }
        LabCommand::Inspect { subsystem } => {
            let state = load_state(&repository)?;
            let http = reqwest::Client::builder().timeout(Duration::from_secs(120)).build()?;
            for node in &state.topology.nodes {
                let url = format!("{}{}", node.diagnostics_endpoint(&state.settings), subsystem.path());
                let request = match subsystem {
                    Subsystem::Sandbox => http.post(url),
                    Subsystem::Network | Subsystem::Attachments => http.get(url),
                };
                let facts: serde_json::Value = request.send().await?.error_for_status()?.json().await?;
                println!("{}:", node.name());
                match facts.get("ruleset").and_then(serde_json::Value::as_str) {
                    Some(ruleset) => {
                        println!("{}\n{ruleset}", serde_json::to_string_pretty(&facts["ports"])?);
                    }
                    None => println!("{}", serde_json::to_string_pretty(&facts)?),
                }
            }
            Ok(())
        }
        LabCommand::Maas(command) => maas_lab(&repository, command).await,
        LabCommand::Contract => {
            let state = load_state(&repository)?;
            let http = reqwest::Client::builder().timeout(Duration::from_secs(120)).build()?;
            for node in &state.topology.nodes {
                let url = format!("{}{HYPERVISOR_CONTRACT_PATH}", node.diagnostics_endpoint(&state.settings));
                let outcome: ContractOutcome = http.post(url).send().await?.json().await?;
                match outcome.violation {
                    None => println!(
                        "{}: the Cloud Hypervisor adapter honours the hypervisor contract",
                        node.name()
                    ),
                    Some(violation) => bail!("{}: {violation}", node.name()),
                }
            }
            Ok(())
        }
    }
}

fn run(arguments: Arguments) -> anyhow::Result<()> {
    match arguments.command {
        Command::Simulate { seeds, first_seed, scenario, crash_roles } => {
            simulate(first_seed..first_seed + seeds, scenario.as_deref(), crash_roles)
        }
        Command::Replay { seed, scenario, crash_roles } => {
            simulate(seed..seed + 1, scenario.as_deref(), crash_roles)
        }
        Command::Scenarios => {
            for definition in amafu_scenarios::all() {
                let levels: Vec<String> = definition.levels.iter().map(ToString::to_string).collect();
                println!("{} [{}]", definition.name, levels.join(", "));
            }
            Ok(())
        }
        Command::Scale { nodes, vms, read_rate, read_seconds, state_in_memory } => {
            scale(nodes, vms, read_rate, read_seconds, state_in_memory)
        }
        Command::Lab(command) => {
            tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(lab(command))
        }
    }
}

fn main() -> ExitCode {
    let _ = rustls::crypto::ring::default_provider().install_default();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("error")),
        )
        .init();
    match run(Arguments::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}
