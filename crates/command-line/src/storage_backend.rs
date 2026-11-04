use amafu_api::ApiClient;
use amafu_domain::kinds::storage_backend::{
    StorageBackend, StorageBackendSpecification, StorageProviderSpecification,
};
use amafu_domain::{Object, ObjectName};
use clap::Subcommand;

use crate::output::OutputFormat;
use crate::resources::{CommonCommand, Tabulated, create, kebab_name, run_common};

#[derive(clap::Args)]
#[group(required = true, multiple = false)]
pub struct ProviderChoice {
    #[arg(long, help = "Keep volumes on the disk of the node that runs the VM")]
    local_disk: bool,
    #[arg(long, value_name = "SERVER:EXPORT", help = "Keep volumes on an NFS export")]
    nfs: Option<String>,
    #[arg(
        long,
        value_name = "FILE",
        help = "Read the provider from a YAML file, for NVMe/TCP targets and Ceph clusters"
    )]
    provider_file: Option<std::path::PathBuf>,
}

#[derive(Subcommand)]
pub enum StorageBackendCommand {
    #[command(about = "Declare where volumes are kept")]
    Create {
        name: ObjectName,
        #[command(flatten)]
        provider: ProviderChoice,
    },
    #[command(flatten)]
    Common(CommonCommand),
}

impl Tabulated for StorageBackend {
    const HEADINGS: &'static [&'static str] = &["NAME", "PHASE", "PROVIDER"];

    fn row(backend: &Object<Self>) -> Vec<String> {
        let provider = match &backend.specification.provider {
            StorageProviderSpecification::LocalDisk => String::from("local disk"),
            StorageProviderSpecification::Nfs { server, export } => format!("nfs {server}:{export}"),
            StorageProviderSpecification::NvmeTcp { management, pool, .. } => {
                format!("nvme-tcp {management} {pool}")
            }
            StorageProviderSpecification::CephRbd { management, pool, .. } => {
                format!("ceph-rbd {management} {pool}")
            }
            StorageProviderSpecification::Plugin(plugin) => format!("plugin {}", plugin.module.url),
        };
        vec![backend.metadata.name.to_string(), kebab_name(backend.status.phase), provider]
    }
}

fn provider(choice: ProviderChoice) -> anyhow::Result<StorageProviderSpecification> {
    if let Some(path) = choice.provider_file {
        return Ok(serde_yaml_ng::from_str(&std::fs::read_to_string(path)?)?);
    }
    match choice.nfs {
        Some(location) => {
            let (server, export) = location
                .split_once(':')
                .ok_or_else(|| anyhow::anyhow!("give the NFS location as SERVER:EXPORT"))?;
            Ok(StorageProviderSpecification::Nfs { server: server.to_owned(), export: export.to_owned() })
        }
        None => Ok(StorageProviderSpecification::LocalDisk),
    }
}

pub async fn run(
    client: &ApiClient,
    command: StorageBackendCommand,
    format: OutputFormat,
) -> anyhow::Result<()> {
    match command {
        StorageBackendCommand::Create { name, provider: choice } => {
            let specification = StorageBackendSpecification { provider: provider(choice)? };
            create::<StorageBackend>(client, &name, &specification, format).await
        }
        StorageBackendCommand::Common(command) => run_common::<StorageBackend>(client, command, format).await,
    }
}
