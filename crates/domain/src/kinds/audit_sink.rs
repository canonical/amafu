use serde::{Deserialize, Serialize};

use crate::access::ClusterPolicy;
use crate::resource::{Resource, Scope};
use crate::validation::{Validate, ValidationError};

const LONGEST_FIELD: usize = 1024;
const LONGEST_MESSAGE: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditSink;

impl Resource for AuditSink {
    const KIND: &'static str = "AuditSink";
    const PLURAL_NAME: &'static str = "audit-sinks";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Cluster;
    const CLUSTER_POLICY: ClusterPolicy = ClusterPolicy::AdminOnly;
    type Specification = AuditSinkSpecification;
    type Status = AuditSinkStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum AuditSinkSpecification {
    Syslog {
        address: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        server_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        certificate_authority: Option<String>,
    },
    ObjectStorage {
        endpoint: String,
        bucket: String,
        region: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        prefix: String,
        access_key: String,
        secret_key: String,
    },
}

fn is_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= LONGEST_FIELD && !value.chars().any(char::is_control)
}

fn is_word(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= LONGEST_FIELD
        && value.chars().all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
}

impl Validate for AuditSinkSpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        match self {
            Self::Syslog { address, server_name, certificate_authority } => {
                error.check(
                    address
                        .rsplit_once(':')
                        .is_some_and(|(host, port)| !host.is_empty() && port.parse::<u16>().is_ok()),
                    "specification.address",
                    "must be a host and a port",
                );
                error.check(
                    server_name.is_some() == certificate_authority.is_some(),
                    "specification.server_name",
                    "TLS needs both a server name and a certificate authority",
                );
                error.check(
                    certificate_authority
                        .as_deref()
                        .is_none_or(|pem| pem.trim_start().starts_with("-----BEGIN CERTIFICATE-----")),
                    "specification.certificate_authority",
                    "must be a PEM certificate",
                );
            }
            Self::ObjectStorage { endpoint, bucket, region, prefix, access_key, secret_key } => {
                error.check(
                    (endpoint.starts_with("https://") || endpoint.starts_with("http://"))
                        && is_text(endpoint),
                    "specification.endpoint",
                    "must be an HTTP URL",
                );
                error.check(is_word(bucket), "specification.bucket", "must be a bucket name");
                error.check(is_word(region), "specification.region", "must be a region name");
                error.check(
                    prefix.is_empty() || (is_text(prefix) && !prefix.split('/').any(|part| part == "..")),
                    "specification.prefix",
                    "must be a path without '..'",
                );
                error.check(
                    is_text(access_key) && is_text(secret_key),
                    "specification.access_key",
                    "must be printable",
                );
            }
        }
        error.into_result()
    }

    fn validate_update(&self, _previous: &Self) -> Result<(), ValidationError> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AuditSinkStatus {
    #[serde(default)]
    pub next_sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<String>,
}

impl AuditSinkStatus {
    #[must_use]
    pub fn delivered_through(&self, last_sequence: u64) -> Self {
        Self { next_sequence: last_sequence + 1, last_failure: None }
    }

    #[must_use]
    pub fn failed(&self, failure: &str) -> Self {
        let mut last_failure = failure.to_owned();
        last_failure.truncate(LONGEST_MESSAGE);
        Self { last_failure: Some(last_failure), ..self.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_position_only_moves_forward_past_what_was_delivered() {
        let status = AuditSinkStatus::default().delivered_through(41);
        assert_eq!(status.next_sequence, 42);
        let failed = status.failed("the collector refused");
        assert_eq!(failed.next_sequence, 42);
        assert_eq!(failed.delivered_through(50), AuditSinkStatus { next_sequence: 51, last_failure: None });
    }

    #[test]
    fn malformed_sinks_are_refused() {
        let syslog = |address: &str, server_name: Option<&str>| AuditSinkSpecification::Syslog {
            address: address.to_owned(),
            server_name: server_name.map(str::to_owned),
            certificate_authority: None,
        };
        assert_eq!(syslog("collector.example:6514", None).validate(), Ok(()));
        assert!(syslog("collector.example", None).validate().is_err());
        assert!(syslog("collector.example:6514", Some("collector")).validate().is_err());
        let storage = AuditSinkSpecification::ObjectStorage {
            endpoint: String::from("https://objects.example"),
            bucket: String::from("amafu-audit"),
            region: String::from("us-east-1"),
            prefix: String::from("../escape"),
            access_key: String::from("key"),
            secret_key: String::from("secret"),
        };
        assert!(storage.validate().is_err());
    }
}
