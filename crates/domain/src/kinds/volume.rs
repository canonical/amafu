use serde::{Deserialize, Serialize};

use crate::name::{NodeName, ObjectName};
use crate::resource::{Resource, Scope};
use crate::validation::{Validate, ValidationError};

pub const MAXIMUM_SIZE_GIBIBYTES: u32 = 16 * 1024;
pub const BYTES_PER_GIBIBYTE: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume;

impl Resource for Volume {
    const KIND: &'static str = "Volume";
    const PLURAL_NAME: &'static str = "volumes";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Project;
    type Specification = VolumeSpecification;
    type Status = VolumeStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeSpecification {
    pub size_gibibytes: u32,
    pub backend: ObjectName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_image: Option<ObjectName>,
}

impl VolumeSpecification {
    #[must_use]
    pub fn size_bytes(&self) -> u64 {
        u64::from(self.size_gibibytes) * BYTES_PER_GIBIBYTE
    }
}

impl Validate for VolumeSpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(
            (1..=MAXIMUM_SIZE_GIBIBYTES).contains(&self.size_gibibytes),
            "specification.size_gibibytes",
            "must be between 1 and 16384",
        );
        error.into_result()
    }

    fn validate_update(&self, previous: &Self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(self == previous, "specification", "a volume cannot be changed once created");
        error.into_result()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvisionedVolume {
    pub identifier: String,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<NodeName>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VolumePhase {
    #[default]
    Pending,
    Available,
    Attached,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct VolumeStatus {
    pub phase: VolumePhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provisioned: Option<ProvisionedVolume>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attached_to: Option<ObjectName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl VolumeStatus {
    #[must_use]
    pub fn is_reserved_for(&self, vm: &ObjectName) -> bool {
        self.provisioned.is_some() && self.attached_to.as_ref() == Some(vm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volume(size_gibibytes: u32) -> VolumeSpecification {
        VolumeSpecification { size_gibibytes, backend: ObjectName::new("local").unwrap(), source_image: None }
    }

    #[test]
    fn sizes_are_bounded() {
        assert_eq!(volume(1).validate(), Ok(()));
        assert!(volume(0).validate().is_err());
        assert!(volume(MAXIMUM_SIZE_GIBIBYTES + 1).validate().is_err());
        assert_eq!(volume(2).size_bytes(), 2 * BYTES_PER_GIBIBYTE);
    }

    #[test]
    fn a_volume_is_immutable() {
        assert!(volume(2).validate_update(&volume(1)).is_err());
        assert_eq!(volume(1).validate_update(&volume(1)), Ok(()));
    }
}
