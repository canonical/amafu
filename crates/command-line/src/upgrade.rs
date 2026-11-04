use std::path::PathBuf;
use std::time::Duration;

use amafu_api::ApiClient;
use amafu_domain::kinds::cluster_version::{CLUSTER_VERSION_NAME, ClusterVersion};
use amafu_domain::kinds::node::Node;
use amafu_domain::kinds::operation::{Operation, OperationKind, OperationSpecification, StepSpecification};
use amafu_domain::node_image::NodeImage;
use amafu_domain::upgrade::{UpgradePlan, WavePolicy, inventory};
use anyhow::{Context, bail};
use clap::{Args, Subcommand};

use crate::operation;
use crate::output::{OutputFormat, Table, print_structured};

#[derive(Args)]
pub struct PlanArguments {
    #[arg(long, default_value_t = WavePolicy::default().canary_count, help = "Nodes upgraded alone before the rest")]
    canary_count: usize,
    #[arg(long, default_value_t = WavePolicy::default().health_deadline_seconds, help = "How long a node has to come back healthy")]
    health_deadline_seconds: u64,
    #[arg(long, default_value_t = WavePolicy::default().soak_seconds, help = "How long to watch the cluster between waves")]
    soak_seconds: u64,
    #[arg(long, help = "Show the steps without starting them")]
    dry_run: bool,
    #[arg(long, value_parser = seconds, help = "Wait this many seconds for the operation to finish")]
    wait: Option<Duration>,
}

fn seconds(text: &str) -> Result<Duration, String> {
    text.parse::<u64>().map(Duration::from_secs).map_err(|error| error.to_string())
}

impl PlanArguments {
    fn policy(&self) -> WavePolicy {
        WavePolicy {
            canary_count: self.canary_count,
            health_deadline_seconds: self.health_deadline_seconds,
            soak_seconds: self.soak_seconds,
        }
    }
}

#[derive(Subcommand)]
pub enum UpgradeCommand {
    #[command(about = "Roll a signed image across the cluster, one node at a time")]
    Start {
        #[arg(long, help = "The signed image manifest")]
        image: PathBuf,
        #[command(flatten)]
        plan: PlanArguments,
    },
    #[command(about = "Go back to the image the cluster ran before the last upgrade")]
    Rollback {
        #[command(flatten)]
        plan: PlanArguments,
    },
    #[command(about = "Enable the features of the current image; the cluster can no longer go back")]
    Finalize,
    #[command(about = "Show which image each node runs")]
    Status,
}

async fn cluster_version(client: &ApiClient) -> anyhow::Result<amafu_domain::Object<ClusterVersion>> {
    client.get::<ClusterVersion>(CLUSTER_VERSION_NAME).await.context("reading the cluster version")
}

fn print_plan(plan: &UpgradePlan) {
    println!("{} to {}", if plan.is_rollback { "roll back" } else { "upgrade" }, plan.image.identity());
    for (index, step) in plan.steps().iter().enumerate() {
        let subject = match step {
            StepSpecification::Evacuate { node }
            | StepSpecification::WriteInactiveSlot { node }
            | StepSpecification::RebootIntoSlot { node }
            | StepSpecification::AwaitHealthy { node }
            | StepSpecification::Uncordon { node } => node.to_string(),
            _ => String::new(),
        };
        println!("{:>4}  {:<20} {subject}", index + 1, step.name());
    }
}

async fn ensure_no_upgrade_runs(client: &ApiClient) -> anyhow::Result<()> {
    let running: Vec<String> = client
        .list::<Operation>()
        .await?
        .into_iter()
        .filter(|operation| {
            operation.specification.kind == OperationKind::Upgrade && !operation.status.is_finished()
        })
        .map(|operation| operation.metadata.name.to_string())
        .collect();
    if !running.is_empty() {
        bail!("an upgrade is already running: {}", running.join(", "));
    }
    Ok(())
}

async fn start(
    client: &ApiClient,
    plan: UpgradePlan,
    arguments: &PlanArguments,
    format: OutputFormat,
) -> anyhow::Result<()> {
    if arguments.dry_run {
        print_plan(&plan);
        return Ok(());
    }
    ensure_no_upgrade_runs(client).await?;
    let prefix = if plan.is_rollback { "rollback" } else { "upgrade" };
    let name = operation::unique_name(prefix, &plan.image.version)?;
    operation::start(client, &name, &OperationSpecification::upgrade(plan), arguments.wait, format).await
}

fn print_status(nodes: &[amafu_domain::Object<Node>]) {
    let mut table = Table::new(vec!["NODE", "RUNS", "SLOT", "CONFIRMED", "INACTIVE SLOT", "CORDONED"]);
    for node in nodes {
        let boot = node.status.boot.as_ref();
        table.add(vec![
            node.metadata.name.to_string(),
            boot.and_then(|boot| boot.booted_image.as_ref())
                .map_or_else(|| String::from("-"), ToString::to_string),
            boot.map_or_else(|| String::from("-"), |boot| boot.booted_slot.letter().to_owned()),
            boot.map_or_else(|| String::from("-"), |boot| boot.is_confirmed().to_string()),
            boot.and_then(|boot| boot.inactive_image.as_ref())
                .map_or_else(|| String::from("-"), ToString::to_string),
            node.specification.is_cordoned.to_string(),
        ]);
    }
    print!("{}", table.render());
}

pub async fn run(client: &ApiClient, command: UpgradeCommand, format: OutputFormat) -> anyhow::Result<()> {
    match command {
        UpgradeCommand::Start { image, plan: arguments } => {
            let image: NodeImage = serde_json::from_slice(
                &std::fs::read(&image).with_context(|| format!("reading {}", image.display()))?,
            )
            .context("the image manifest is malformed")?;
            let nodes = client.list::<Node>().await?;
            let plan = UpgradePlan::upgrade(&inventory(&nodes, &image), image, &arguments.policy());
            start(client, plan, &arguments, format).await
        }
        UpgradeCommand::Rollback { plan: arguments } => {
            let version = cluster_version(client).await?;
            let Some(previous) = version.specification.previous_image else {
                bail!("the cluster was finalized or never upgraded, so there is nothing to go back to");
            };
            let nodes = client.list::<Node>().await?;
            let plan = UpgradePlan::rollback(&inventory(&nodes, &previous), previous, &arguments.policy());
            start(client, plan, &arguments, format).await
        }
        UpgradeCommand::Finalize => {
            let version = cluster_version(client).await?;
            let nodes = client.list::<Node>().await?;
            let behind: Vec<String> = nodes
                .iter()
                .filter(|node| !node.status.runs_confirmed(&version.specification.image))
                .map(|node| node.metadata.name.to_string())
                .collect();
            if !behind.is_empty() {
                bail!(
                    "these nodes do not run {} yet: {}",
                    version.specification.image.identity(),
                    behind.join(", ")
                );
            }
            let finalized = version.specification.finalized();
            let updated = client
                .update::<ClusterVersion>(CLUSTER_VERSION_NAME, &finalized, Some(version.metadata.generation))
                .await?;
            print_structured(&updated, format)
        }
        UpgradeCommand::Status => {
            let version = cluster_version(client).await?;
            if format != OutputFormat::Table {
                return print_structured(&version, format);
            }
            println!(
                "cluster image {} at feature level {}{}",
                version.specification.image.identity(),
                version.specification.finalized_feature_level,
                version
                    .specification
                    .previous_image
                    .as_ref()
                    .map(|previous| format!(", can go back to {}", previous.identity()))
                    .unwrap_or_default()
            );
            print_status(&client.list::<Node>().await?);
            Ok(())
        }
    }
}
