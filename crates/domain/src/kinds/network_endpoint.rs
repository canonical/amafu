use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

use crate::access::ProjectPolicy;
use crate::addressing::MacAddress;
use crate::kinds::vm::{MAXIMUM_NETWORK_INTERFACES, MAXIMUM_SECURITY_GROUPS_PER_INTERFACE};
use crate::name::{NodeName, ObjectName};
use crate::object_id::ObjectId;
use crate::resource::{Resource, Scope};
use crate::validation::{Validate, ValidationError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkEndpoint;

impl Resource for NetworkEndpoint {
    const KIND: &'static str = "NetworkEndpoint";
    const PLURAL_NAME: &'static str = "network-endpoints";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Project;
    const PROJECT_POLICY: ProjectPolicy = ProjectPolicy::ManagedByNodes;
    type Specification = NetworkEndpointSpecification;
    type Status = NetworkEndpointStatus;
}

impl NetworkEndpoint {
    #[must_use]
    pub fn name_for(vm_id: ObjectId, index: u8) -> ObjectName {
        ObjectName::new(format!("endpoint-{}-{index}", vm_id.as_uuid().simple()))
            .unwrap_or_else(|_| unreachable!())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkEndpointSpecification {
    pub network: ObjectName,
    pub vm: ObjectName,
    pub vm_id: ObjectId,
    pub index: u8,
    pub mac: MacAddress,
    pub address: Ipv4Addr,
    pub node: NodeName,
    pub node_address: Ipv4Addr,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub security_groups: Vec<ObjectName>,
}

impl Validate for NetworkEndpointSpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(
            usize::from(self.index) < MAXIMUM_NETWORK_INTERFACES,
            "specification.index",
            "must be below 4",
        );
        error.check(
            self.security_groups.len() <= MAXIMUM_SECURITY_GROUPS_PER_INTERFACE,
            "specification.security_groups",
            "must hold at most 8 security groups",
        );
        error.into_result()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NetworkEndpointStatus {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_names_are_valid_and_distinct_per_interface() {
        let vm_id = ObjectId::from_uuid(uuid::Uuid::new_v4());
        let first = NetworkEndpoint::name_for(vm_id, 0);
        assert_ne!(first, NetworkEndpoint::name_for(vm_id, 1));
        assert!(first.as_str().starts_with("endpoint-"));
    }
}
