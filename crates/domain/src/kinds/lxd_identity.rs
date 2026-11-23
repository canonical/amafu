use serde::{Deserialize, Serialize};

use crate::access::ClusterPolicy;
use crate::name::ObjectName;
use crate::resource::{Resource, Scope};
use crate::validation::{Validate, ValidationError};

pub const LXD_IDENTITY_NAME: &str = "cluster";

#[must_use]
pub fn lxd_identity_name() -> ObjectName {
    ObjectName::new(LXD_IDENTITY_NAME).unwrap_or_else(|_| unreachable!())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LxdIdentity;

impl Resource for LxdIdentity {
    const KIND: &'static str = "LxdIdentity";
    const PLURAL_NAME: &'static str = "lxd-identities";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Cluster;
    const CLUSTER_POLICY: ClusterPolicy = ClusterPolicy::AdminOnly;
    type Specification = LxdIdentitySpecification;
    type Status = LxdIdentityStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LxdIdentitySpecification {
    pub certificate_pem: String,
    pub private_key_pem: String,
}

impl Validate for LxdIdentitySpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(
            self.certificate_pem.contains("BEGIN CERTIFICATE"),
            "specification.certificate_pem",
            "must be a PEM certificate",
        );
        error.check(
            self.private_key_pem.contains("PRIVATE KEY"),
            "specification.private_key_pem",
            "must be a PEM private key",
        );
        error.into_result()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LxdIdentityStatus {}
