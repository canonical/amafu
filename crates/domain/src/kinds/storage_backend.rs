use std::net::SocketAddrV4;

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
    Nfs {
        server: String,
        export: String,
    },
    NvmeTcp {
        management: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        management_user: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        management_password: Option<String>,
        addresses: Vec<SocketAddrV4>,
        pool: String,
    },
    CephRbd {
        monitors: Vec<SocketAddrV4>,
        pool: String,
        user: String,
        key: String,
        management: String,
        management_user: String,
        management_password: String,
        command_endpoint: String,
        command_user: String,
        command_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command_certificate_authority: Option<String>,
    },
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

fn is_certificate(text: &str) -> bool {
    text.trim_start().starts_with("-----BEGIN CERTIFICATE-----")
}

fn is_management_url(value: &str) -> bool {
    (value.starts_with("http://") || value.starts_with("https://"))
        && value.len() <= 1024
        && !value.chars().any(|character| character.is_control() || character.is_whitespace())
}

fn is_plain_word(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.chars().all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
}

fn is_secret(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.chars().any(|character| character.is_control() || character == ',')
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
        match &self.provider {
            StorageProviderSpecification::LocalDisk => {}
            StorageProviderSpecification::Nfs { server, export } => {
                error.check(
                    is_host(server),
                    "specification.provider.server",
                    "must be a host name or IPv4 address",
                );
                error.check(
                    is_export_path(export),
                    "specification.provider.export",
                    "must be an absolute path",
                );
            }
            StorageProviderSpecification::NvmeTcp {
                management,
                management_user,
                management_password,
                addresses,
                pool,
            } => {
                error.check(
                    is_management_url(management),
                    "specification.provider.management",
                    "must be an HTTP URL",
                );
                error.check(
                    management_user.as_deref().is_none_or(is_plain_word)
                        && management_password.as_deref().is_none_or(is_secret),
                    "specification.provider.management_user",
                    "the management credentials must be printable",
                );
                error.check(
                    !addresses.is_empty(),
                    "specification.provider.addresses",
                    "must name at least one target address",
                );
                error.check(
                    is_plain_word(pool),
                    "specification.provider.pool",
                    "must be a logical volume store name",
                );
            }
            StorageProviderSpecification::CephRbd {
                monitors,
                pool,
                user,
                key,
                management,
                management_user,
                management_password,
                command_endpoint,
                command_user,
                command_key,
                command_certificate_authority,
            } => {
                error.check(
                    command_certificate_authority.as_deref().is_none_or(is_certificate),
                    "specification.provider.command_certificate_authority",
                    "must be a PEM certificate",
                );
                error.check(
                    is_management_url(command_endpoint),
                    "specification.provider.command_endpoint",
                    "must be an HTTP URL",
                );
                error.check(
                    is_plain_word(command_user) && is_secret(command_key),
                    "specification.provider.command_user",
                    "the command credentials must be printable",
                );
                error.check(
                    !monitors.is_empty(),
                    "specification.provider.monitors",
                    "must name at least one monitor",
                );
                error.check(is_plain_word(pool), "specification.provider.pool", "must be a pool name");
                error.check(
                    is_plain_word(user) && is_secret(key),
                    "specification.provider.user",
                    "must be a Ceph client and its key",
                );
                error.check(
                    is_management_url(management),
                    "specification.provider.management",
                    "must be an HTTP URL",
                );
                error.check(
                    is_plain_word(management_user) && is_secret(management_password),
                    "specification.provider.management_user",
                    "the management credentials must be printable",
                );
            }
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
    fn block_backends_need_a_management_endpoint_and_targets() {
        let nvme = |addresses: Vec<SocketAddrV4>| StorageBackendSpecification {
            provider: StorageProviderSpecification::NvmeTcp {
                management: String::from("http://10.216.0.41:9009"),
                management_user: Some(String::from("amafu")),
                management_password: Some(String::from("secret")),
                addresses,
                pool: String::from("amafu"),
            },
        };
        assert_eq!(nvme(vec!["10.216.0.41:4420".parse().unwrap()]).validate(), Ok(()));
        assert!(nvme(Vec::new()).validate().is_err());
        let ceph = StorageBackendSpecification {
            provider: StorageProviderSpecification::CephRbd {
                monitors: vec!["10.216.0.42:6789".parse().unwrap()],
                pool: String::from("amafu"),
                user: String::from("amafu"),
                key: String::from("AQBzZXJ2ZXItc2lkZS1rZXk="),
                management: String::from("ftp://ceph"),
                management_user: String::from("admin"),
                management_password: String::from("secret"),
                command_endpoint: String::from("https://ceph:8003"),
                command_user: String::from("amafu"),
                command_key: String::from("secret"),
                command_certificate_authority: None,
            },
        };
        assert!(ceph.validate().is_err());
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
