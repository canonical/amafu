use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use amafu_domain::ObjectId;
use amafu_domain::kinds::image::ImageSource;
use amafu_interfaces::{ActionError, CachedImage, ContainerConfiguration, ImageRequest, ImageStore};

use crate::lock::lock;

pub const UNREACHABLE_HOST: &str = "unreachable.example";
const IMAGE_SIZE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Default)]
pub struct FakeImageStore {
    cached: Mutex<BTreeMap<ObjectId, CachedImage>>,
}

impl FakeImageStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

fn fetch(request: &ImageRequest) -> Result<CachedImage, ActionError> {
    match &request.source {
        ImageSource::Disk { url, .. } if url.contains(UNREACHABLE_HOST) => {
            Err(ActionError::invalid(format!("{url} cannot be fetched")))
        }
        ImageSource::Disk { .. } => Ok(CachedImage {
            path: PathBuf::from(format!("/var/lib/amafu/images/{}.qcow2", request.image_id)),
            size_bytes: IMAGE_SIZE_BYTES,
            container: None,
        }),
        ImageSource::Container { .. } => Ok(CachedImage {
            path: PathBuf::from(format!("/var/lib/amafu/images/{}.erofs", request.image_id)),
            size_bytes: IMAGE_SIZE_BYTES,
            container: Some(ContainerConfiguration {
                command: vec![String::from("/bin/sh")],
                ..ContainerConfiguration::default()
            }),
        }),
    }
}

#[async_trait::async_trait]
impl ImageStore for FakeImageStore {
    async fn ensure(&self, request: &ImageRequest) -> Result<CachedImage, ActionError> {
        if let Some(cached) = lock(&self.cached).get(&request.image_id) {
            return Ok(cached.clone());
        }
        let cached = fetch(request)?;
        lock(&self.cached).insert(request.image_id, cached.clone());
        Ok(cached)
    }

    async fn cached(&self, image_id: &ObjectId) -> Result<Option<CachedImage>, ActionError> {
        Ok(lock(&self.cached).get(image_id).cloned())
    }

    async fn remove(&self, image_id: &ObjectId) -> Result<(), ActionError> {
        lock(&self.cached).remove(image_id);
        Ok(())
    }
}
