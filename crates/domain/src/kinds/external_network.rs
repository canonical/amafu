use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

use crate::addressing::Ipv4Network;
use crate::resource::{Resource, Scope};
use crate::validation::{Validate, ValidationError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalNetwork;

impl Resource for ExternalNetwork {
    const KIND: &'static str = "ExternalNetwork";
    const PLURAL_NAME: &'static str = "external-networks";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Cluster;
    type Specification = ExternalNetworkSpecification;
    type Status = ExternalNetworkStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalNetworkSpecification {
    pub subnet: Ipv4Network,
    pub first_address: Ipv4Addr,
    pub last_address: Ipv4Addr,
}

impl ExternalNetworkSpecification {
    #[must_use]
    pub fn contains(&self, address: Ipv4Addr) -> bool {
        (self.first_address..=self.last_address).contains(&address)
    }

    pub fn addresses(&self) -> impl Iterator<Item = Ipv4Addr> {
        (u32::from(self.first_address)..=u32::from(self.last_address)).map(Ipv4Addr::from)
    }
}

impl Validate for ExternalNetworkSpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(
            self.subnet.is_assignable(self.first_address) && self.subnet.is_assignable(self.last_address),
            "specification",
            "the address range must lie inside the subnet",
        );
        error.check(
            self.first_address <= self.last_address,
            "specification.last_address",
            "must not come before the first address",
        );
        error.into_result()
    }

    fn validate_update(&self, previous: &Self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(self == previous, "specification", "an external network cannot be changed once created");
        error.into_result()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ExternalNetworkStatus {}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(first: &str, last: &str) -> ExternalNetworkSpecification {
        ExternalNetworkSpecification {
            subnet: "192.0.2.0/24".parse().unwrap(),
            first_address: first.parse().unwrap(),
            last_address: last.parse().unwrap(),
        }
    }

    #[test]
    fn the_range_must_be_inside_the_subnet_and_in_order() {
        assert_eq!(range("192.0.2.100", "192.0.2.149").validate(), Ok(()));
        assert!(range("192.0.2.100", "198.51.100.1").validate().is_err());
        assert!(range("192.0.2.149", "192.0.2.100").validate().is_err());
        assert_eq!(range("192.0.2.100", "192.0.2.102").addresses().count(), 3);
    }
}
