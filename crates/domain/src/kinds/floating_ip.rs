use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

use crate::kinds::vm::MAXIMUM_NETWORK_INTERFACES;
use crate::name::{NodeName, ObjectName};
use crate::object_id::ObjectId;
use crate::resource::{Resource, Scope};
use crate::validation::{Validate, ValidationError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FloatingIp;

impl Resource for FloatingIp {
    const KIND: &'static str = "FloatingIp";
    const PLURAL_NAME: &'static str = "floating-ips";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Project;
    type Specification = FloatingIpSpecification;
    type Status = FloatingIpStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FloatingIpTarget {
    pub vm: ObjectName,
    #[serde(default)]
    pub interface: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FloatingIpSpecification {
    pub external_network: ObjectName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<Ipv4Addr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<FloatingIpTarget>,
}

impl Validate for FloatingIpSpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(
            self.target
                .as_ref()
                .is_none_or(|target| usize::from(target.interface) < MAXIMUM_NETWORK_INTERFACES),
            "specification.target.interface",
            "must be below 4",
        );
        error.into_result()
    }

    fn validate_update(&self, previous: &Self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(
            self.external_network == previous.external_network,
            "specification.external_network",
            "cannot change",
        );
        error.check(
            self.address.is_none() || self.address == previous.address,
            "specification.address",
            "cannot change",
        );
        error.into_result()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FloatingIpPhase {
    #[default]
    Pending,
    Allocated,
    Bound,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FloatingIpBinding {
    pub node: NodeName,
    pub vm_id: ObjectId,
    pub private_address: Ipv4Addr,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FloatingIpStatus {
    pub phase: FloatingIpPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<Ipv4Addr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<FloatingIpBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_target_can_move_but_the_address_stays() {
        let previous = FloatingIpSpecification {
            external_network: ObjectName::new("lab").unwrap(),
            address: None,
            target: Some(FloatingIpTarget { vm: ObjectName::new("web-1").unwrap(), interface: 0 }),
        };
        let moved = FloatingIpSpecification {
            target: Some(FloatingIpTarget { vm: ObjectName::new("web-2").unwrap(), interface: 0 }),
            ..previous.clone()
        };
        assert_eq!(moved.validate_update(&previous), Ok(()));
        let readdressed =
            FloatingIpSpecification { address: Some(Ipv4Addr::new(192, 0, 2, 9)), ..previous.clone() };
        assert!(readdressed.validate_update(&previous).is_err());
    }
}
