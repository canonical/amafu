use std::collections::BTreeMap;
use std::net::SocketAddrV4;

use serde::{Deserialize, Serialize};

use crate::digest::Sha256Digest;
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
    Plugin(PluginBackend),
}

pub const LONGEST_PLUGIN_CONFIGURATION: usize = 64 * 1024;
pub const MOST_PLUGIN_ENDPOINTS: usize = 16;
pub const MOST_PLUGIN_SECRETS: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginModule {
    pub url: String,
    pub digest: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginBackend {
    pub module: PluginModule,
    #[serde(default)]
    pub configuration: serde_json::Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub endpoints: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secrets: BTreeMap<String, String>,
}

impl PluginBackend {
    #[must_use]
    pub fn allows(&self, url: &str) -> bool {
        self.endpoints.iter().any(|endpoint| {
            url == endpoint
                || url.strip_prefix(endpoint.as_str()).is_some_and(|rest| {
                    endpoint.ends_with('/') || rest.starts_with('/') || rest.starts_with('?')
                })
        })
    }

    fn check(&self, error: &mut ValidationError) {
        error.check(
            is_management_url(&self.module.url),
            "specification.provider.module.url",
            "must be an HTTP URL",
        );
        error.check(
            self.configuration.to_string().len() <= LONGEST_PLUGIN_CONFIGURATION,
            "specification.provider.configuration",
            "must be at most 64 KiB of JSON",
        );
        error.check(
            self.endpoints.len() <= MOST_PLUGIN_ENDPOINTS
                && self.endpoints.iter().all(|endpoint| is_management_url(endpoint)),
            "specification.provider.endpoints",
            "must be at most 16 HTTP URLs",
        );
        error.check(
            self.secrets.len() <= MOST_PLUGIN_SECRETS
                && self.secrets.iter().all(|(name, value)| is_plain_word(name) && is_secret(value)),
            "specification.provider.secrets",
            "must be at most 16 named, printable secrets",
        );
    }
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
            StorageProviderSpecification::Plugin(plugin) => plugin.check(&mut error),
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

    fn plugin(endpoints: &[&str]) -> PluginBackend {
        PluginBackend {
            module: PluginModule {
                url: String::from("https://plugins.example/a.wasm"),
                digest: "a".repeat(64).parse().unwrap(),
            },
            configuration: serde_json::Value::Null,
            endpoints: endpoints.iter().map(|endpoint| (*endpoint).to_owned()).collect(),
            secrets: BTreeMap::new(),
        }
    }

    #[test]
    fn a_plugin_reaches_only_below_the_endpoints_it_declares() {
        let backend = plugin(&["https://target.example:8080", "https://other.example/api/"]);
        assert!(backend.allows("https://target.example:8080"));
        assert!(backend.allows("https://target.example:8080/rpc"));
        assert!(backend.allows("https://other.example/api/volumes"));
        assert!(!backend.allows("https://target.example:80801/rpc"));
        assert!(!backend.allows("https://target.example:8080.attacker.example/"));
        assert!(!backend.allows("https://other.example/apix"));
        assert!(!plugin(&[]).allows("https://target.example:8080"));
    }

    #[test]
    fn a_plugin_backend_with_too_much_or_malformed_configuration_is_refused() {
        let specification = |plugin: PluginBackend| StorageBackendSpecification {
            provider: StorageProviderSpecification::Plugin(plugin),
        };
        assert_eq!(specification(plugin(&["https://target.example"])).validate(), Ok(()));
        assert!(specification(plugin(&["ftp://target.example"])).validate().is_err());
        let mut secretive = plugin(&[]);
        secretive.secrets.insert(String::from("pass word"), String::from("x"));
        assert!(specification(secretive).validate().is_err());
    }

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
