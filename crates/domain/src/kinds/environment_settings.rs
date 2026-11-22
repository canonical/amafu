use serde::{Deserialize, Serialize};

use crate::access::ClusterPolicy;
use crate::digest::OciReference;
use crate::kinds::image::ImageSource;
use crate::name::ObjectName;
use crate::resource::{Resource, Scope};
use crate::validation::{Validate, ValidationError};

pub const ENVIRONMENT_SETTINGS_NAME: &str = "cluster";

#[must_use]
pub fn environment_settings_name() -> ObjectName {
    ObjectName::new(ENVIRONMENT_SETTINGS_NAME).unwrap_or_else(|_| unreachable!())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentSettings;

impl Resource for EnvironmentSettings {
    const KIND: &'static str = "EnvironmentSettings";
    const PLURAL_NAME: &'static str = "environment-settings";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Cluster;
    const CLUSTER_POLICY: ClusterPolicy = ClusterPolicy::AdminOnly;
    type Specification = EnvironmentSettingsSpecification;
    type Status = EnvironmentSettingsStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentSettingsSpecification {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ubuntu_image: Option<ImageSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub juju_runner_image: Option<OciReference>,
}

impl Validate for EnvironmentSettingsSpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(
            !matches!(self.ubuntu_image, Some(ImageSource::Container { .. })),
            "specification.ubuntu_image",
            "must be a disk image",
        );
        error.into_result()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EnvironmentSettingsStatus {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ubuntu_image_is_a_disk() {
        let reference: OciReference =
            format!("registry.example/runner@sha256:{}", "a".repeat(64)).parse().unwrap();
        let container = EnvironmentSettingsSpecification {
            ubuntu_image: Some(ImageSource::Container { reference }),
            juju_runner_image: None,
        };
        assert!(container.validate().is_err());
        assert_eq!(EnvironmentSettingsSpecification::default().validate(), Ok(()));
    }
}
