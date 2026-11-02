use amafu_api::ApiClient;
use amafu_domain::kinds::image::{Image, ImageSource, ImageSpecification};
use amafu_domain::{Object, ObjectName, OciReference, Sha256Digest};
use clap::Subcommand;

use crate::output::OutputFormat;
use crate::resources::{CommonCommand, Tabulated, create, kebab_name, run_common};

#[derive(clap::Args)]
#[group(required = true, multiple = false)]
pub struct ImageOrigin {
    #[arg(long, value_name = "URL", requires = "sha256", help = "Fetch a raw or qcow2 disk image")]
    disk_url: Option<String>,
    #[arg(long, value_name = "REFERENCE", help = "Pull a container image pinned by digest")]
    container: Option<OciReference>,
}

#[derive(Subcommand)]
pub enum ImageCommand {
    #[command(about = "Register an image that nodes fetch and cache")]
    Create {
        name: ObjectName,
        #[command(flatten)]
        origin: ImageOrigin,
        #[arg(long, value_name = "HEX", help = "The SHA-256 digest the disk image must have")]
        sha256: Option<Sha256Digest>,
    },
    #[command(flatten)]
    Common(CommonCommand),
}

impl Tabulated for Image {
    const HEADINGS: &'static [&'static str] = &["NAME", "PHASE", "SOURCE", "SIZE", "CACHED ON"];

    fn row(image: &Object<Self>) -> Vec<String> {
        let source = match &image.specification.source {
            ImageSource::Disk { url, .. } => format!("disk {url}"),
            ImageSource::Container { reference } => format!("container {reference}"),
        };
        let cached_on: Vec<String> = image.status.cached_on.iter().map(ToString::to_string).collect();
        vec![
            image.metadata.name.to_string(),
            kebab_name(image.status.phase),
            source,
            format!("{} MiB", image.status.size_bytes / (1024 * 1024)),
            cached_on.join(","),
        ]
    }
}

pub async fn run(client: &ApiClient, command: ImageCommand, format: OutputFormat) -> anyhow::Result<()> {
    match command {
        ImageCommand::Create { name, origin, sha256 } => {
            let source = match (origin.disk_url, origin.container, sha256) {
                (Some(url), None, Some(sha256)) => ImageSource::Disk { url, sha256 },
                (None, Some(reference), None) => ImageSource::Container { reference },
                _ => anyhow::bail!("give --disk-url with --sha256, or --container alone"),
            };
            create::<Image>(client, &name, &ImageSpecification { source }, format).await
        }
        ImageCommand::Common(command) => run_common::<Image>(client, command, format).await,
    }
}
