use serde::{Deserialize, Serialize};

use crate::addressing::Ipv4Network;
use crate::name::ObjectName;
use crate::resource::{Resource, Scope};
use crate::validation::{Validate, ValidationError};

pub const MAXIMUM_RULES: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityGroup;

impl Resource for SecurityGroup {
    const KIND: &'static str = "SecurityGroup";
    const PLURAL_NAME: &'static str = "security-groups";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Project;
    type Specification = SecurityGroupSpecification;
    type Status = SecurityGroupStatus;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Direction {
    Ingress,
    Egress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Protocol {
    Any,
    Tcp,
    Udp,
    Icmp,
}

impl Protocol {
    #[must_use]
    pub fn has_ports(self) -> bool {
        matches!(self, Self::Tcp | Self::Udp)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortRange {
    pub first: u16,
    pub last: u16,
}

impl PortRange {
    #[must_use]
    pub fn contains(&self, port: u16) -> bool {
        (self.first..=self.last).contains(&port)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Remote {
    Any,
    Addresses { network: Ipv4Network },
    SecurityGroup { name: ObjectName },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityGroupRule {
    pub direction: Direction,
    pub protocol: Protocol,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ports: Option<PortRange>,
    pub remote: Remote,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityGroupSpecification {
    #[serde(default)]
    pub rules: Vec<SecurityGroupRule>,
}

impl Validate for SecurityGroupSpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(self.rules.len() <= MAXIMUM_RULES, "specification.rules", "must hold at most 128 rules");
        for (index, rule) in self.rules.iter().enumerate() {
            let field = format!("specification.rules[{index}].ports");
            match rule.ports {
                Some(_) if !rule.protocol.has_ports() => {
                    error.add(field, "only tcp and udp rules have ports");
                }
                Some(ports) if ports.first == 0 || ports.first > ports.last => {
                    error.add(field, "must be a range of ports from 1 to 65535 with first <= last");
                }
                _ => {}
            }
        }
        error.into_result()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SecurityGroupStatus {}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(protocol: Protocol, ports: Option<PortRange>) -> SecurityGroupRule {
        SecurityGroupRule { direction: Direction::Ingress, protocol, ports, remote: Remote::Any }
    }

    #[test]
    fn port_ranges_apply_only_to_tcp_and_udp() {
        let web = PortRange { first: 80, last: 80 };
        let valid = SecurityGroupSpecification {
            rules: vec![rule(Protocol::Tcp, Some(web)), rule(Protocol::Icmp, None)],
        };
        assert_eq!(valid.validate(), Ok(()));
        let icmp_ports = SecurityGroupSpecification { rules: vec![rule(Protocol::Icmp, Some(web))] };
        assert!(icmp_ports.validate().is_err());
        let backwards = SecurityGroupSpecification {
            rules: vec![rule(Protocol::Udp, Some(PortRange { first: 90, last: 80 }))],
        };
        assert!(backwards.validate().is_err());
    }

    #[test]
    fn rules_read_naturally_in_json() {
        let body = r#"{"rules":[{"direction":"ingress","protocol":"tcp","ports":{"first":22,"last":22},"remote":{"type":"addresses","network":"10.0.0.0/8"}}]}"#;
        let specification: SecurityGroupSpecification = serde_json::from_str(body).unwrap();
        assert_eq!(specification.validate(), Ok(()));
    }
}
