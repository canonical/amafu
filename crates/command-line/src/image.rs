use amafu_api::ApiClient;
use amafu_domain::kinds::image::{Image, ImageSource, ImageSpecification};
use amafu_domain::{Object, ObjectName, OciReference, Sha256Digest};
use clap::Subcommand;

use amafu_interfaces::CatalogKind;

use crate::catalog;
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
    #[command(about = "List the images Canonical publishes for VMs and containers")]
    Catalog {
        #[arg(long, help = "Only container images")]
        containers: bool,
    },
    #[command(about = "Register an image from the catalog")]
    Import {
        #[arg(value_name = "ID", help = "A catalog ID such as ubuntu/24.04")]
        id: String,
        #[arg(long, help = "Import the container image rather than the VM disk")]
        container: bool,
        #[arg(long, help = "Name it differently from the catalog's suggestion")]
        name: Option<ObjectName>,
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
        ImageCommand::Catalog { containers } => {
            let entries: Vec<_> = client
                .image_catalog()
                .await?
                .into_iter()
                .filter(|entry| !containers || entry.kind == CatalogKind::Container)
                .collect();
            catalog::print_catalog(&entries, format)
        }
        ImageCommand::Import { id, container, name } => {
            let kind = if container { CatalogKind::Container } else { CatalogKind::VirtualMachine };
            let entry = client
                .image_catalog()
                .await?
                .into_iter()
                .find(|entry| entry.kind == kind && entry.id == id)
                .ok_or_else(|| anyhow::anyhow!("{id} is not in the catalog (see amafu image catalog)"))?;
            let name = catalog::import(client, &entry, name).await?;
            run_common::<Image>(client, CommonCommand::Show { name: name.to_string() }, format).await
        }
        ImageCommand::Common(command) => run_common::<Image>(client, command, format).await,
    }
}
