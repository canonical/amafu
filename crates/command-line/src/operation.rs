use std::time::Duration;

use amafu_api::ApiClient;
use amafu_domain::kinds::operation::{Operation, OperationSpecification, VmReference};
use amafu_domain::kinds::vm::Vm;
use amafu_domain::{NodeName, Object, ObjectName};
use anyhow::Context;
use clap::Subcommand;

use crate::output::{OutputFormat, print_structured};
use crate::resources::{Tabulated, print_objects};
use crate::step_plans::{self, plan_columns, unique_name};

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

impl Tabulated for Operation {
    const HEADINGS: &'static [&'static str] = &["NAME", "KIND", "PROGRESS", "STEP", "STATE"];

    fn row(operation: &Object<Self>) -> Vec<String> {
        let [progress, step, state] = plan_columns(operation);
        vec![
            operation.metadata.name.to_string(),
            operation.specification.kind.to_string(),
            progress,
            step,
            state,
        ]
    }
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

pub async fn start(
    client: &ApiClient,
    name: &ObjectName,
    specification: &OperationSpecification,
    wait: Option<Duration>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    step_plans::submit::<Operation>(client, name, specification, wait, format).await
}

pub async fn migrate(
    client: &ApiClient,
    vm: &str,
    destination: Option<NodeName>,
    wait: Option<Duration>,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let found = client.get::<Vm>(vm).await?;
    let project = found.metadata.project.context("the VM belongs to no project")?;
    let reference = VmReference { project, name: found.metadata.name };
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
        OperationCommand::List => print_objects(&client.list::<Operation>().await?, format),
        OperationCommand::Show { name } => print_structured(&client.get::<Operation>(&name).await?, format),
        OperationCommand::Pause { name } => print_objects(
            &[change(client, &name, |specification| specification.is_paused = true).await?],
            format,
        ),
        OperationCommand::Resume { name } => print_objects(
            &[change(client, &name, |specification| specification.is_paused = false).await?],
            format,
        ),
        OperationCommand::Cancel { name } => {
            crate::step_plans::cancel::<Operation>(client, &name, format).await
        }
        OperationCommand::Delete { name } => {
            print_objects(&[client.delete::<Operation>(&name).await?], format)
        }
    }
}
