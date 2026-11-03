use serde::{Deserialize, Serialize};

use crate::resource::{Resource, Scope};
use crate::validation::{Validate, ValidationError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageBackend;

impl Resource for StorageBackend {
    const KIND: &'static str = "StorageBackend";
    const PLURAL_NAME: &'static str = "storage-backends";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Cluster;
    type Specification = StorageBackendSpecification;
    type Status = StorageBackendStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum StorageProviderSpecification {
    LocalDisk,
    Nfs { server: String, export: String },
}

impl StorageProviderSpecification {
    #[must_use]
    pub fn is_file_based(&self) -> bool {
        matches!(self, Self::LocalDisk | Self::Nfs { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageBackendSpecification {
    pub provider: StorageProviderSpecification,
}

fn is_host(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value.chars().all(|character| character.is_ascii_alphanumeric() || ".-".contains(character))
}

fn is_export_path(value: &str) -> bool {
    value.starts_with('/')
        && value.len() <= 1024
        && !value.split('/').any(|component| component == "..")
        && !value.chars().any(|character| character.is_control() || character == ',')
}

impl Validate for StorageBackendSpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        if let StorageProviderSpecification::Nfs { server, export } = &self.provider {
            error.check(
                is_host(server),
                "specification.provider.server",
                "must be a host name or IPv4 address",
            );
            error.check(is_export_path(export), "specification.provider.export", "must be an absolute path");
        }
        error.into_result()
    }

    fn validate_update(&self, previous: &Self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(self.provider == previous.provider, "specification.provider", "cannot change");
        error.into_result()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StorageBackendPhase {
    #[default]
    Unknown,
    Ready,
    Unreachable,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StorageBackendStatus {
    pub phase: StorageBackendPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl StorageBackendStatus {
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.phase == StorageBackendPhase::Ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nfs(server: &str, export: &str) -> StorageBackendSpecification {
        StorageBackendSpecification {
            provider: StorageProviderSpecification::Nfs {
                server: server.to_owned(),
                export: export.to_owned(),
            },
        }
    }

    #[test]
    fn nfs_backends_need_a_host_and_an_absolute_export() {
        assert_eq!(nfs("10.215.2.60", "/srv/amafu").validate(), Ok(()));
        assert!(nfs("", "/srv/amafu").validate().is_err());
        assert!(nfs("filer", "srv/amafu").validate().is_err());
        assert!(nfs("filer", "/srv/../etc").validate().is_err());
        assert!(nfs("filer", "/srv/amafu,ro").validate().is_err());
    }
}
