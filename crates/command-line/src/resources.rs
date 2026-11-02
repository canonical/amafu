use amafu_api::ApiClient;
use amafu_domain::{Object, Resource};
use clap::Subcommand;
use serde::Serialize;

use crate::output::{OutputFormat, Table, print_structured};

pub trait Tabulated: Resource + Sized {
    const HEADINGS: &'static [&'static str];

    fn row(object: &Object<Self>) -> Vec<String>;
}

#[derive(Subcommand)]
pub enum CommonCommand {
    #[command(about = "List them")]
    List,
    #[command(about = "Show one")]
    Show { name: String },
    #[command(about = "Delete one")]
    Delete { name: String },
}

#[must_use]
pub fn kebab_name(value: impl Serialize) -> String {
    serde_json::to_value(value).ok().and_then(|value| value.as_str().map(str::to_owned)).unwrap_or_default()
}

#[must_use]
pub fn or_dash(value: Option<String>) -> String {
    value.unwrap_or_else(|| String::from("-"))
}

pub fn print_objects<R: Tabulated>(objects: &[Object<R>], format: OutputFormat) -> anyhow::Result<()>
where
    Object<R>: Serialize,
{
    if format != OutputFormat::Table {
        return print_structured(&objects, format);
    }
    let mut table = Table::new(R::HEADINGS.to_vec());
    for object in objects {
        table.add(R::row(object));
    }
    print!("{}", table.render());
    Ok(())
}

pub async fn create<R: Tabulated>(
    client: &ApiClient,
    name: &amafu_domain::ObjectName,
    specification: &R::Specification,
    format: OutputFormat,
) -> anyhow::Result<()>
where
    Object<R>: Serialize,
{
    print_objects(&[client.create::<R>(name, specification).await?], format)
}

pub async fn run_common<R: Tabulated>(
    client: &ApiClient,
    command: CommonCommand,
    format: OutputFormat,
) -> anyhow::Result<()>
where
    Object<R>: Serialize,
{
    match command {
        CommonCommand::List => print_objects(&client.list::<R>().await?, format),
        CommonCommand::Show { name } => print_structured(&client.get::<R>(&name).await?, format),
        CommonCommand::Delete { name } => print_objects(&[client.delete::<R>(&name).await?], format),
    }
}
