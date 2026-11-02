use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::digest::{OciReference, Sha256Digest};
use crate::name::NodeName;
use crate::resource::{Resource, Scope};
use crate::validation::{Validate, ValidationError};

const MAXIMUM_URL_LENGTH: usize = 2048;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image;

impl Resource for Image {
    const KIND: &'static str = "Image";
    const PLURAL_NAME: &'static str = "images";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Project;
    type Specification = ImageSpecification;
    type Status = ImageStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ImageSource {
    Disk { url: String, sha256: Sha256Digest },
    Container { reference: OciReference },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageSpecification {
    pub source: ImageSource,
}

impl ImageSpecification {
    #[must_use]
    pub fn is_container(&self) -> bool {
        matches!(self.source, ImageSource::Container { .. })
    }

    #[must_use]
    pub fn is_disk(&self) -> bool {
        matches!(self.source, ImageSource::Disk { .. })
    }
}

impl Validate for ImageSpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        if let ImageSource::Disk { url, .. } = &self.source {
            error.check(
                url.starts_with("https://")
                    && url.len() <= MAXIMUM_URL_LENGTH
                    && !url.chars().any(char::is_whitespace),
                "specification.source.url",
                "must be an https URL of at most 2048 characters",
            );
        }
        error.into_result()
    }

    fn validate_update(&self, previous: &Self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(
            self.source == previous.source,
            "specification.source",
            "cannot change; create a new image",
        );
        error.into_result()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImagePhase {
    #[default]
    Pending,
    Ready,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ImageStatus {
    pub phase: ImagePhase,
    #[serde(default)]
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub cached_on: BTreeSet<NodeName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl ImageStatus {
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.phase == ImagePhase::Ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk(url: &str) -> ImageSpecification {
        let sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".parse().unwrap();
        ImageSpecification { source: ImageSource::Disk { url: url.to_owned(), sha256 } }
    }

    #[test]
    fn disk_images_must_come_from_https() {
        assert_eq!(disk("https://cloud-images.ubuntu.com/minimal/image.img").validate(), Ok(()));
        assert!(disk("http://example.com/image.img").validate().is_err());
        assert!(disk("https://example.com/an image.img").validate().is_err());
    }

    #[test]
    fn the_source_cannot_change() {
        let first = disk("https://example.com/one.img");
        let second = disk("https://example.com/two.img");
        assert!(second.validate_update(&first).is_err());
        assert_eq!(first.validate_update(&first), Ok(()));
    }

    #[test]
    fn a_container_image_is_written_as_a_pinned_reference() {
        let body = r#"{"source":{"type":"container","reference":"docker.io/library/busybox@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}}"#;
        let specification: ImageSpecification = serde_json::from_str(body).unwrap();
        assert!(specification.is_container());
        let unpinned = r#"{"source":{"type":"container","reference":"docker.io/library/busybox:latest"}}"#;
        assert!(serde_json::from_str::<ImageSpecification>(unpinned).is_err());
    }
}
