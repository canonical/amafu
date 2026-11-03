use serde::{Deserialize, Serialize};

use crate::access::ClusterPolicy;
use crate::digest::Sha256Digest;
use crate::resource::{Resource, Scope};
use crate::validation::{Validate, ValidationError};

pub const MAXIMUM_REASON_LENGTH: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateRevocation;

impl Resource for CertificateRevocation {
    const KIND: &'static str = "CertificateRevocation";
    const PLURAL_NAME: &'static str = "certificate-revocations";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Cluster;
    const CLUSTER_POLICY: ClusterPolicy = ClusterPolicy::AdminOnly;
    type Specification = CertificateRevocationSpecification;
    type Status = CertificateRevocationStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CertificateRevocationSpecification {
    pub fingerprint: Sha256Digest,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

impl Validate for CertificateRevocationSpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(
            self.reason.len() <= MAXIMUM_REASON_LENGTH && !self.reason.chars().any(char::is_control),
            "specification.reason",
            "must be at most 1024 printable characters",
        );
        error.into_result()
    }

    fn validate_update(&self, previous: &Self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(self.fingerprint == previous.fingerprint, "specification.fingerprint", "cannot change");
        error.into_result()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CertificateRevocationStatus {}
