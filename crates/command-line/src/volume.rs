use amafu_api::ApiClient;
use amafu_domain::kinds::volume::{Volume, VolumeSpecification};
use amafu_domain::{Object, ObjectName};
use clap::Subcommand;

use crate::output::OutputFormat;
use crate::resources::{CommonCommand, Tabulated, create, kebab_name, or_dash, run_common};

#[derive(Subcommand)]
pub enum VolumeCommand {
    #[command(about = "Create a volume, empty or cloned from an image")]
    Create {
        name: ObjectName,
        #[arg(long = "size", value_name = "GIBIBYTES")]
        size_gibibytes: u32,
        #[arg(long, value_name = "STORAGE_BACKEND")]
        backend: ObjectName,
        #[arg(long = "from-image", value_name = "IMAGE")]
        source_image: Option<ObjectName>,
    },
    #[command(flatten)]
    Common(CommonCommand),
}

impl Tabulated for Volume {
    const HEADINGS: &'static [&'static str] = &["NAME", "PHASE", "SIZE", "BACKEND", "IMAGE", "ATTACHED TO"];

    fn row(volume: &Object<Self>) -> Vec<String> {
        vec![
            volume.metadata.name.to_string(),
            kebab_name(volume.status.phase),
            format!("{} GiB", volume.specification.size_gibibytes),
            volume.specification.backend.to_string(),
            or_dash(volume.specification.source_image.as_ref().map(ToString::to_string)),
            or_dash(volume.status.attached_to.as_ref().map(ToString::to_string)),
        ]
    }
}

pub async fn run(client: &ApiClient, command: VolumeCommand, format: OutputFormat) -> anyhow::Result<()> {
    match command {
        VolumeCommand::Create { name, size_gibibytes, backend, source_image } => {
            let specification = VolumeSpecification { size_gibibytes, backend, source_image };
            create::<Volume>(client, &name, &specification, format).await
        }
        VolumeCommand::Common(command) => run_common::<Volume>(client, command, format).await,
    }
}
