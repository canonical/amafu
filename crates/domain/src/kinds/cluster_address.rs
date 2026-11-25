use std::fmt;
use std::net::Ipv4Addr;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::access::ClusterPolicy;
use crate::addressing::Ipv4Network;
use crate::name::ObjectName;
use crate::resource::{Resource, Scope};
use crate::validation::{Validate, ValidationError};

pub const CLUSTER_ADDRESS_NAME: &str = "cluster";
const MAXIMUM_PREFIX_LENGTH: u8 = 30;

#[must_use]
pub fn cluster_address_name() -> ObjectName {
    ObjectName::new(CLUSTER_ADDRESS_NAME).unwrap_or_else(|_| unreachable!())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterAddress;

impl Resource for ClusterAddress {
    const KIND: &'static str = "ClusterAddress";
    const PLURAL_NAME: &'static str = "cluster-addresses";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Cluster;
    const CLUSTER_POLICY: ClusterPolicy = ClusterPolicy::SharedInfrastructure;
    type Specification = ClusterAddressSpecification;
    type Status = ClusterAddressStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterAddressSpecification {
    pub address: Ipv4Addr,
    pub prefix_length: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<String>,
}

impl ClusterAddressSpecification {
    #[must_use]
    pub fn network(&self) -> Option<Ipv4Network> {
        let host_bits = u32::MAX.checked_shr(u32::from(self.prefix_length)).unwrap_or(0);
        let network_address = Ipv4Addr::from(u32::from(self.address) & !host_bits);
        Ipv4Network::new(network_address, self.prefix_length).ok()
    }
}

impl ClusterAddressSpecification {
    fn is_host_address(&self) -> bool {
        let Some(network) =
            self.network().filter(|_| (1..=MAXIMUM_PREFIX_LENGTH).contains(&self.prefix_length))
        else {
            return false;
        };
        self.address != network.network_address() && self.address != network.broadcast_address()
    }
}

impl fmt::Display for ClusterAddressSpecification {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}", self.address, self.prefix_length)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0} is not an address with its network's prefix length, such as 10.20.0.50/24")]
pub struct MalformedClusterAddress(String);

impl FromStr for ClusterAddressSpecification {
    type Err = MalformedClusterAddress;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let malformed = || MalformedClusterAddress(text.to_owned());
        let (address, prefix_length) = text.split_once('/').ok_or_else(malformed)?;
        let specification = Self {
            address: address.parse().map_err(|_| malformed())?,
            prefix_length: prefix_length.parse().map_err(|_| malformed())?,
            interface: None,
        };
        specification.validate().map_err(|_| malformed())?;
        Ok(specification)
    }
}

impl Validate for ClusterAddressSpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(
            self.is_host_address(),
            "specification.address",
            "must be a host address of the nodes' network, with that network's prefix length (at most 30)",
        );
        if let Some(interface) = &self.interface {
            error.check(
                !interface.is_empty() && interface.len() < 16,
                "specification.interface",
                "must be a network interface name of 1 to 15 characters",
            );
        }
        error.into_result()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ClusterAddressStatus {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterAddressCertificate;

impl Resource for ClusterAddressCertificate {
    const KIND: &'static str = "ClusterAddressCertificate";
    const PLURAL_NAME: &'static str = "cluster-address-certificates";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Cluster;
    const CLUSTER_POLICY: ClusterPolicy = ClusterPolicy::NodeSecret;
    type Specification = ClusterAddressCertificateSpecification;
    type Status = ClusterAddressStatus;
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterAddressCertificateSpecification {
    pub certificate_pem: String,
    pub private_key_pem: String,
}

impl fmt::Debug for ClusterAddressCertificateSpecification {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("ClusterAddressCertificateSpecification").finish_non_exhaustive()
    }
}

impl Validate for ClusterAddressCertificateSpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(
            self.certificate_pem.contains("BEGIN CERTIFICATE"),
            "specification.certificate_pem",
            "must hold a PEM certificate chain",
        );
        error.check(
            self.private_key_pem.contains("PRIVATE KEY"),
            "specification.private_key_pem",
            "must hold a PEM private key",
        );
        error.into_result()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardSessionKey;

impl Resource for DashboardSessionKey {
    const KIND: &'static str = "DashboardSessionKey";
    const PLURAL_NAME: &'static str = "dashboard-session-keys";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Cluster;
    const CLUSTER_POLICY: ClusterPolicy = ClusterPolicy::NodeSecret;
    type Specification = DashboardSessionKeySpecification;
    type Status = ClusterAddressStatus;
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardSessionKeySpecification {
    pub key_hexadecimal: String,
}

impl fmt::Debug for DashboardSessionKeySpecification {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("DashboardSessionKeySpecification").finish_non_exhaustive()
    }
}

impl Validate for DashboardSessionKeySpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(
            self.key_hexadecimal.len() == 64
                && self.key_hexadecimal.chars().all(|digit| digit.is_ascii_hexdigit()),
            "specification.key_hexadecimal",
            "must be 32 bytes written as 64 hexadecimal digits",
        );
        error.into_result()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(text: &str, prefix_length: u8) -> ClusterAddressSpecification {
        ClusterAddressSpecification { address: text.parse().unwrap(), prefix_length, interface: None }
    }

    #[test]
    fn a_cluster_address_is_a_host_of_its_network() {
        assert!(address("10.20.0.50", 24).validate().is_ok());
        assert_eq!(address("10.20.0.50", 24).to_string(), "10.20.0.50/24");
        assert!(address("10.20.0.0", 24).validate().is_err());
        assert!(address("10.20.0.255", 24).validate().is_err());
        assert!(address("10.20.0.50", 33).validate().is_err());
        assert!(address("10.20.0.50", 32).validate().is_err());
    }

    #[test]
    fn a_cluster_address_is_written_as_an_address_and_its_prefix_length() {
        assert_eq!("10.20.0.50/24".parse::<ClusterAddressSpecification>(), Ok(address("10.20.0.50", 24)));
        assert!("10.20.0.50".parse::<ClusterAddressSpecification>().is_err());
        assert!("10.20.0.0/24".parse::<ClusterAddressSpecification>().is_err());
    }

    #[test]
    fn secrets_never_appear_in_debug_output() {
        let certificate = ClusterAddressCertificateSpecification {
            certificate_pem: String::from("-----BEGIN CERTIFICATE-----"),
            private_key_pem: String::from("-----BEGIN PRIVATE KEY----- secret"),
        };
        assert!(!format!("{certificate:?}").contains("secret"));
        let key = DashboardSessionKeySpecification { key_hexadecimal: "ab".repeat(32) };
        assert!(!format!("{key:?}").contains("abab"));
    }
}
