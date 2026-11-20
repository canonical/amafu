use std::collections::BTreeMap;

use amafu_api::ApiClient;
use amafu_domain::ObjectName;
use amafu_domain::kinds::image::{Image, ImageSpecification};
use amafu_domain::kinds::storage_backend::StorageBackend;
use amafu_domain::kinds::volume::{Volume, VolumeSpecification};
use amafu_domain::labels::ROOT_OF;
use amafu_interfaces::{CatalogEntry, CatalogKind};
use anyhow::Context;

use crate::output::{OutputFormat, Table, print_structured};

pub const DEFAULT_DISK_GIBIBYTES: u32 = 20;

pub fn print_catalog(entries: &[CatalogEntry], format: OutputFormat) -> anyhow::Result<()> {
    if format != OutputFormat::Table {
        return print_structured(&entries, format);
    }
    let mut table = Table::new(vec!["ID", "TITLE", "FOR", "IMAGE NAME", "PUBLISHED"]);
    for entry in entries {
        table.add(vec![
            entry.id.clone(),
            entry.title.clone(),
            String::from(match entry.kind {
                CatalogKind::VirtualMachine => "vm",
                CatalogKind::Container => "container",
            }),
            entry.name.clone(),
            entry.published.clone().unwrap_or_else(|| String::from("-")),
        ]);
    }
    crate::output::emit(&table.render())?;
    Ok(())
}

async fn find(client: &ApiClient, id: &str, kind: CatalogKind) -> anyhow::Result<CatalogEntry> {
    client
        .image_catalog()
        .await?
        .into_iter()
        .find(|entry| entry.kind == kind && (entry.id == id || entry.name == id))
        .with_context(|| {
            format!("{id} is neither an image of this project nor in the catalog (see amafu image catalog)")
        })
}

pub async fn import(
    client: &ApiClient,
    entry: &CatalogEntry,
    name: Option<ObjectName>,
) -> anyhow::Result<ObjectName> {
    let name = match name {
        Some(name) => name,
        None => ObjectName::new(entry.name.as_str())?,
    };
    if client.get::<Image>(name.as_str()).await.is_err() {
        client.create::<Image>(&name, &ImageSpecification { source: entry.source.clone() }).await?;
    }
    Ok(name)
}

pub async fn resolve(client: &ApiClient, image: &str, kind: CatalogKind) -> anyhow::Result<ObjectName> {
    match client.get::<Image>(image).await {
        Ok(found) => Ok(found.metadata.name),
        Err(error) if error.is_not_found() => import(client, &find(client, image, kind).await?, None).await,
        Err(error) => Err(error.into()),
    }
}

async fn default_backend(client: &ApiClient) -> anyhow::Result<ObjectName> {
    client
        .list::<StorageBackend>()
        .await?
        .into_iter()
        .map(|backend| backend.metadata.name)
        .next()
        .context("the cluster has no storage backend; pass --storage-backend or create one")
}

pub struct RootDisk {
    pub image: String,
    pub size_gibibytes: u32,
    pub backend: Option<ObjectName>,
    pub is_kept: bool,
}

pub async fn create_root_volume(
    client: &ApiClient,
    vm: &ObjectName,
    disk: RootDisk,
) -> anyhow::Result<ObjectName> {
    let image = resolve(client, &disk.image, CatalogKind::VirtualMachine).await?;
    let backend = match disk.backend {
        Some(backend) => backend,
        None => default_backend(client).await?,
    };
    let name = ObjectName::new(format!("{vm}-root"))?;
    let specification =
        VolumeSpecification { size_gibibytes: disk.size_gibibytes, backend, source_image: Some(image) };
    let labels =
        if disk.is_kept { BTreeMap::new() } else { BTreeMap::from([(ROOT_OF.to_owned(), vm.to_string())]) };
    client
        .create_labelled::<Volume>(&name, labels, &specification)
        .await
        .with_context(|| format!("creating {name}"))?;
    Ok(name)
}
