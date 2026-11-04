mod layers;
mod registry;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use amafu_domain::kinds::image::ImageSource;
use amafu_domain::{ObjectId, OciReference, Sha256Digest};
use amafu_interfaces::{ActionError, CachedImage, ExternalTool, ImageRequest, ImageStore};
use amafu_system::files::write_durably;
use futures::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use self::registry::{Descriptor, RegistryClient};

const RECORD_FILE: &str = "image.json";
const DISK_FILE: &str = "disk.img";
const ROOT_DIRECTORY: &str = "root";
const ROOT_IMAGE: &str = "root.erofs";
const MAXIMUM_DISK_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const MAXIMUM_LAYER_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const GZIP_LAYER_SUFFIX: &str = "tar+gzip";
const PLAIN_LAYER_SUFFIX: &str = ".tar";

pub struct CachedImages {
    root: PathBuf,
    mkfs_erofs: ExternalTool,
    http: reqwest::Client,
}

fn retryable(action: &'static str) -> impl Fn(std::io::Error) -> ActionError {
    move |error| ActionError::retryable(format!("{action}: {error}"))
}

pub fn https_client() -> Result<reqwest::Client, ActionError> {
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(ActionError::fatal)?
        .with_root_certificates(roots)
        .with_no_client_auth();
    reqwest::Client::builder().use_preconfigured_tls(tls).https_only(true).build().map_err(ActionError::fatal)
}

async fn download(
    response: reqwest::Response,
    path: &Path,
    limit: u64,
    expected: &Sha256Digest,
) -> Result<u64, ActionError> {
    let temporary = path.with_extension("partial");
    let mut file = tokio::fs::File::create(&temporary).await.map_err(retryable("creating a download"))?;
    let mut hasher = Sha256::new();
    let mut written: u64 = 0;
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(|error| ActionError::retryable(format!("downloading: {error}")))?;
        written += chunk.len() as u64;
        if written > limit {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(ActionError::invalid(format!("the download is larger than {limit} bytes")));
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await.map_err(retryable("writing a download"))?;
    }
    file.sync_all().await.map_err(retryable("flushing a download"))?;
    if hex::encode(hasher.finalize()) != expected.hexadecimal() {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(ActionError::invalid(format!("the download does not match {expected}")));
    }
    tokio::fs::rename(&temporary, path).await.map_err(retryable("keeping a download"))?;
    Ok(written)
}

impl CachedImages {
    pub fn new(root: PathBuf, mkfs_erofs: ExternalTool) -> Result<Self, ActionError> {
        Ok(Self { root, mkfs_erofs, http: https_client()? })
    }

    fn directory(&self, image_id: &ObjectId) -> PathBuf {
        self.root.join(image_id.to_string())
    }

    async fn fetch_disk(
        &self,
        directory: &Path,
        url: &str,
        sha256: &Sha256Digest,
    ) -> Result<CachedImage, ActionError> {
        let response = self
            .http
            .get(url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|error| ActionError::retryable(format!("fetching {url}: {error}")))?;
        let path = directory.join(DISK_FILE);
        let size_bytes = download(response, &path, MAXIMUM_DISK_BYTES, sha256).await?;
        Ok(CachedImage { path, size_bytes, container: None })
    }

    async fn unpack(
        &self,
        client: &mut RegistryClient,
        layer: &Descriptor,
        directory: &Path,
    ) -> Result<(), ActionError> {
        let is_gzip = layer.media_type.ends_with(GZIP_LAYER_SUFFIX);
        if !is_gzip && !layer.media_type.ends_with(PLAIN_LAYER_SUFFIX) {
            return Err(ActionError::invalid(format!(
                "layers of type {} are not supported",
                layer.media_type
            )));
        }
        if layer.size > MAXIMUM_LAYER_BYTES {
            return Err(ActionError::invalid("a layer is larger than 4 GiB"));
        }
        let digest: Sha256Digest = layer.digest.parse().map_err(ActionError::invalid)?;
        let response = client.get("blobs", &layer.digest, None).await?;
        let blob = directory.join("layer");
        download(response, &blob, MAXIMUM_LAYER_BYTES, &digest).await?;
        let root = directory.join(ROOT_DIRECTORY);
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let file = std::fs::File::open(&blob)?;
            if is_gzip {
                layers::unpack_layer(&root, flate2::read::GzDecoder::new(file))?;
            } else {
                layers::unpack_layer(&root, file)?;
            }
            std::fs::remove_file(&blob)
        })
        .await
        .map_err(ActionError::fatal)?
        .map_err(|error| ActionError::invalid(format!("unpacking a layer: {error}")))
    }

    async fn build_erofs(&self, directory: &Path) -> Result<PathBuf, ActionError> {
        let image = directory.join(ROOT_IMAGE);
        let output = tokio::process::Command::from(self.mkfs_erofs.command())
            .arg(&image)
            .arg(directory.join(ROOT_DIRECTORY))
            .output()
            .await
            .map_err(retryable("running mkfs.erofs"))?;
        if !output.status.success() {
            return Err(ActionError::retryable(format!(
                "mkfs.erofs failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        tokio::fs::File::open(&image)
            .await
            .map_err(retryable("opening the root image"))?
            .sync_all()
            .await
            .map_err(retryable("flushing the root image"))?;
        tokio::fs::remove_dir_all(directory.join(ROOT_DIRECTORY))
            .await
            .map_err(retryable("removing the unpacked layers"))?;
        Ok(image)
    }

    async fn fetch_container(
        &self,
        directory: &Path,
        reference: &OciReference,
    ) -> Result<CachedImage, ActionError> {
        let mut client = RegistryClient::new(self.http.clone(), reference.clone());
        let manifest = client.manifest().await?;
        let root = directory.join(ROOT_DIRECTORY);
        if root.exists() {
            tokio::fs::remove_dir_all(&root).await.map_err(retryable("clearing an earlier attempt"))?;
        }
        tokio::fs::create_dir_all(&root).await.map_err(retryable("creating the root"))?;
        for layer in &manifest.layers {
            self.unpack(&mut client, layer, directory).await?;
        }
        let path = self.build_erofs(directory).await?;
        let size_bytes = tokio::fs::metadata(&path).await.map_err(retryable("reading the root image"))?.len();
        Ok(CachedImage { path, size_bytes, container: Some(manifest.configuration) })
    }

    async fn record(&self, image_id: &ObjectId) -> Result<Option<CachedImage>, ActionError> {
        match tokio::fs::read(self.directory(image_id).join(RECORD_FILE)).await {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)
                .inspect_err(
                    |error| tracing::warn!(%image_id, %error, "an unreadable image record is ignored"),
                )
                .ok()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(retryable("reading the image record")(error)),
        }
    }
}

#[async_trait::async_trait]
impl ImageStore for CachedImages {
    async fn ensure(&self, request: &ImageRequest) -> Result<CachedImage, ActionError> {
        if let Some(cached) = self.record(&request.image_id).await? {
            return Ok(cached);
        }
        let directory = self.directory(&request.image_id);
        tokio::fs::create_dir_all(&directory).await.map_err(retryable("creating the image directory"))?;
        let cached = match &request.source {
            ImageSource::Disk { url, sha256 } => self.fetch_disk(&directory, url, sha256).await?,
            ImageSource::Container { reference } => self.fetch_container(&directory, reference).await?,
        };
        let record = directory.join(RECORD_FILE);
        let contents = serde_json::to_vec(&cached).map_err(ActionError::fatal)?;
        tokio::task::spawn_blocking(move || write_durably(&record, &contents))
            .await
            .map_err(ActionError::fatal)?
            .map_err(retryable("keeping the image record"))?;
        tracing::info!(image_id = %request.image_id, size_bytes = cached.size_bytes, "cached an image");
        Ok(cached)
    }

    async fn cached(&self, image_id: &ObjectId) -> Result<Option<CachedImage>, ActionError> {
        self.record(image_id).await
    }

    async fn remove(&self, image_id: &ObjectId) -> Result<(), ActionError> {
        match tokio::fs::remove_dir_all(self.directory(image_id)).await {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                Err(retryable("removing an image")(error))
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_image_whose_record_was_cut_short_counts_as_not_cached() {
        let root = tempfile::tempdir().unwrap();
        let images =
            CachedImages::new(root.path().to_path_buf(), ExternalTool::installed("mkfs.erofs")).unwrap();
        let image_id = ObjectId::from_uuid(uuid::Uuid::new_v4());
        std::fs::create_dir_all(images.directory(&image_id)).unwrap();
        std::fs::write(images.directory(&image_id).join(RECORD_FILE), b"").unwrap();

        assert_eq!(images.cached(&image_id).await.unwrap(), None);
    }
}
