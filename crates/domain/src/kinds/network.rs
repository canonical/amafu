use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

use crate::addressing::{Ipv4Network, SegmentId};
use crate::resource::{Resource, Scope};
use crate::validation::{Validate, ValidationError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Network;

impl Resource for Network {
    const KIND: &'static str = "Network";
    const PLURAL_NAME: &'static str = "networks";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Project;
    type Specification = NetworkSpecification;
    type Status = NetworkStatus;
}

fn allowed_by_default() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSpecification {
    pub subnet: Ipv4Network,
    #[serde(default = "allowed_by_default")]
    pub is_outbound_access_allowed: bool,
}

pub const MINIMUM_SUBNET_PREFIX_LENGTH: u8 = 8;
pub const MAXIMUM_SUBNET_PREFIX_LENGTH: u8 = 29;

impl Validate for NetworkSpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(
            (MINIMUM_SUBNET_PREFIX_LENGTH..=MAXIMUM_SUBNET_PREFIX_LENGTH)
                .contains(&self.subnet.prefix_length()),
            "specification.subnet",
            "must have a prefix length between 8 and 29",
        );
        error.into_result()
    }

    fn validate_update(&self, previous: &Self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(self.subnet == previous.subnet, "specification.subnet", "cannot change");
        error.into_result()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkPhase {
    #[default]
    Pending,
    Ready,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NetworkStatus {
    pub phase: NetworkPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<Ipv4Addr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segment_id: Option<SegmentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl NetworkStatus {
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.phase == NetworkPhase::Ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subnets_must_leave_room_for_hosts() {
        let subnet = |text: &str| NetworkSpecification {
            subnet: text.parse().unwrap(),
            is_outbound_access_allowed: true,
        };
        assert_eq!(subnet("10.0.0.0/24").validate(), Ok(()));
        assert!(subnet("10.0.0.0/30").validate().is_err());
        assert!(subnet("0.0.0.0/4").validate().is_err());
    }
}
