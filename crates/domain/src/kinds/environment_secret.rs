use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::access::ClusterPolicy;
use crate::name::{ObjectName, ProjectName};
use crate::resource::{Resource, Scope};
use crate::validation::{Validate, ValidationError};

pub const MAXIMUM_VALUES: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentSecret;

impl Resource for EnvironmentSecret {
    const KIND: &'static str = "EnvironmentSecret";
    const PLURAL_NAME: &'static str = "environment-secrets";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Cluster;
    const CLUSTER_POLICY: ClusterPolicy = ClusterPolicy::AdminOnly;
    type Specification = EnvironmentSecretSpecification;
    type Status = EnvironmentSecretStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentSecretSpecification {
    pub project: ProjectName,
    pub environment: ObjectName,
    #[serde(default)]
    pub values: BTreeMap<String, String>,
}

impl EnvironmentSecretSpecification {
    #[must_use]
    pub fn value(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }
}

impl Validate for EnvironmentSecretSpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(
            self.values.len() <= MAXIMUM_VALUES,
            "specification.values",
            "must hold at most 16 values",
        );
        error.into_result()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EnvironmentSecretStatus {}
