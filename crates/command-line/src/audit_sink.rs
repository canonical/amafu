use std::path::PathBuf;

use amafu_api::ApiClient;
use amafu_domain::kinds::audit_sink::{AuditSink, AuditSinkSpecification};
use amafu_domain::{Object, ObjectName};
use anyhow::Context;
use clap::Subcommand;

use crate::output::OutputFormat;
use crate::resources::{CommonCommand, Tabulated, create, run_common};

#[derive(Subcommand)]
pub enum AuditSinkCommand {
    #[command(about = "Start exporting the audit log")]
    Create {
        name: ObjectName,
        #[arg(long, value_name = "FILE", help = "The sink, as YAML")]
        file: PathBuf,
    },
    #[command(flatten)]
    Common(CommonCommand),
}

impl Tabulated for AuditSink {
    const HEADINGS: &'static [&'static str] = &["NAME", "TYPE", "EXPORTED", "LAST FAILURE"];

    fn row(sink: &Object<Self>) -> Vec<String> {
        let kind = match &sink.specification {
            AuditSinkSpecification::Syslog { address, .. } => format!("syslog {address}"),
            AuditSinkSpecification::ObjectStorage { endpoint, bucket, .. } => {
                format!("object storage {endpoint}/{bucket}")
            }
        };
        vec![
            sink.metadata.name.to_string(),
            kind,
            sink.status.next_sequence.to_string(),
            sink.status.last_failure.clone().unwrap_or_else(|| String::from("-")),
        ]
    }
}

pub async fn run(client: &ApiClient, command: AuditSinkCommand, format: OutputFormat) -> anyhow::Result<()> {
    match command {
        AuditSinkCommand::Create { name, file } => {
            let contents =
                std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
            let specification: AuditSinkSpecification = serde_yaml_ng::from_str(&contents)?;
            create::<AuditSink>(client, &name, &specification, format).await
        }
        AuditSinkCommand::Common(command) => run_common::<AuditSink>(client, command, format).await,
    }
}
