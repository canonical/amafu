use std::time::{Duration, SystemTime, UNIX_EPOCH};

use amafu_api::ApiClient;
use amafu_domain::kinds::operation::{Operation, OperationOutcome, OperationSpecification, VmReference};
use amafu_domain::kinds::vm::Vm;
use amafu_domain::{NodeName, Object, ObjectName};
use anyhow::{Context, bail};
use clap::Subcommand;

use crate::output::{OutputFormat, Table, print_structured};

const POLL_INTERVAL: Duration = Duration::from_millis(500);
const LONGEST_SUBJECT: usize = 40;

#[derive(Subcommand)]
pub enum OperationCommand {
    #[command(about = "List operations")]
    List,
    #[command(about = "Show an operation and its progress")]
    Show { name: String },
    #[command(about = "Stop an operation before its next step")]
    Pause { name: String },
    #[command(about = "Let a paused operation continue")]
    Resume { name: String },
    #[command(about = "Undo an operation if none of its steps is past undoing")]
    Cancel { name: String },
    #[command(about = "Delete a finished operation")]
    Delete { name: String },
}

fn outcome_text(operation: &Object<Operation>) -> String {
    match (&operation.status.outcome, &operation.status.rollback) {
        (Some(OperationOutcome::Succeeded), _) => String::from("succeeded"),
        (Some(OperationOutcome::Cancelled), _) => String::from("cancelled"),
        (Some(OperationOutcome::Failed { reason }), _) => format!("failed: {reason}"),
        (None, Some(_)) => String::from("rolling back"),
        (None, None) if operation.specification.is_paused => String::from("paused"),
        (None, None) => String::from("running"),
    }
}

fn current_step(operation: &Object<Operation>) -> String {
    let specification = &operation.specification;
    let Some(step) = specification.steps.get(operation.status.completed_steps) else {
        return String::from("-");
    };
    let error = operation
        .status
        .current_step
        .as_ref()
        .and_then(|progress| progress.last_error.as_ref())
        .map(|error| format!(" ({error})"))
        .unwrap_or_default();
    format!("{}{error}", step.name())
}

fn print_operations(operations: &[Object<Operation>], format: OutputFormat) -> anyhow::Result<()> {
    if format != OutputFormat::Table {
        return print_structured(&operations, format);
    }
    let mut table = Table::new(vec!["NAME", "KIND", "PROGRESS", "STEP", "STATE"]);
    for operation in operations {
        table.add(vec![
            operation.metadata.name.to_string(),
            operation.specification.kind.to_string(),
            format!("{}/{}", operation.status.completed_steps, operation.specification.steps.len()),
            current_step(operation),
            outcome_text(operation),
        ]);
    }
    print!("{}", table.render());
    Ok(())
}

fn unique_name(prefix: &str, subject: &str) -> anyhow::Result<ObjectName> {
    let stamp =
        SystemTime::now().duration_since(UNIX_EPOCH).map(|elapsed| elapsed.as_millis()).unwrap_or_default();
    let subject: String = subject.chars().take(LONGEST_SUBJECT).collect();
    Ok(ObjectName::new(format!("{prefix}-{subject}-{:x}", stamp & 0xff_ffff_ffff))?)
}

async fn change(
    client: &ApiClient,
    name: &str,
    edit: impl FnOnce(&mut OperationSpecification),
) -> anyhow::Result<Object<Operation>> {
    let operation = client.get::<Operation>(name).await?;
    let mut specification = operation.specification;
    edit(&mut specification);
    Ok(client.update::<Operation>(name, &specification, Some(operation.metadata.generation)).await?)
}

pub async fn wait_until_finished(
    client: &ApiClient,
    name: &str,
    timeout: Duration,
) -> anyhow::Result<Object<Operation>> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let operation = client.get::<Operation>(name).await?;
        if operation.status.is_finished() {
            return Ok(operation);
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("operation {name} has not finished after {timeout:?}: {}", current_step(&operation));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

pub async fn start(
    client: &ApiClient,
    name: &ObjectName,
    specification: &OperationSpecification,
    wait: Option<Duration>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let created = client.create::<Operation>(name, specification).await?;
    let Some(timeout) = wait else {
        return print_operations(&[created], format);
    };
    let finished = wait_until_finished(client, name.as_str(), timeout).await?;
    print_operations(std::slice::from_ref(&finished), format)?;
    match finished.status.outcome {
        Some(OperationOutcome::Succeeded) => Ok(()),
        _ => bail!("operation {name} did not succeed"),
    }
}

pub async fn migrate(
    client: &ApiClient,
    vm: &str,
    destination: Option<NodeName>,
    wait: Option<Duration>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let project = client.project().cloned().context("migrating a VM needs --project")?;
    let reference = VmReference { project, name: ObjectName::new(vm)? };
    let specification = OperationSpecification::migration(reference, destination);
    start(client, &unique_name("migrate", vm)?, &specification, wait, format).await
}

pub async fn evacuate(
    client: &ApiClient,
    node: &NodeName,
    wait: Option<Duration>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let vms: Vec<VmReference> = client
        .list_in_every_project::<Vm>()
        .await?
        .into_iter()
        .filter(|vm| vm.status.node.as_ref() == Some(node))
        .filter_map(|vm| Some(VmReference { project: vm.metadata.project?, name: vm.metadata.name }))
        .collect();
    let specification = OperationSpecification::evacuation(node.clone(), vms);
    start(client, &unique_name("evacuate", node.as_str())?, &specification, wait, format).await
}

pub async fn run(client: &ApiClient, command: OperationCommand, format: OutputFormat) -> anyhow::Result<()> {
    match command {
        OperationCommand::List => print_operations(&client.list::<Operation>().await?, format),
        OperationCommand::Show { name } => print_structured(&client.get::<Operation>(&name).await?, format),
        OperationCommand::Pause { name } => print_operations(
            &[change(client, &name, |specification| specification.is_paused = true).await?],
            format,
        ),
        OperationCommand::Resume { name } => print_operations(
            &[change(client, &name, |specification| specification.is_paused = false).await?],
            format,
        ),
        OperationCommand::Cancel { name } => print_operations(
            &[change(client, &name, |specification| specification.is_cancelled = true).await?],
            format,
        ),
        OperationCommand::Delete { name } => {
            print_operations(&[client.delete::<Operation>(&name).await?], format)
        }
    }
}
