use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::access::ProjectPolicy;
use crate::name::NodeName;
use crate::object_id::ObjectId;
use crate::resource::{Resource, Scope};
use crate::validation::{Validate, ValidationError};

pub const MAXIMUM_LISTED_INSTANCES: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestInventory;

impl Resource for GuestInventory {
    const KIND: &'static str = "GuestInventory";
    const PLURAL_NAME: &'static str = "guest-inventories";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Project;
    const PROJECT_POLICY: ProjectPolicy = ProjectPolicy::ManagedByNodes;
    type Specification = GuestInventorySpecification;
    type Status = GuestInventoryStatus;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GuestInstanceKind {
    Container,
    VirtualMachine,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestInstance {
    pub name: String,
    pub kind: GuestInstanceKind,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "access", rename_all = "kebab-case", deny_unknown_fields)]
pub enum LxdAccess {
    Untrusted,
    Trusted {
        server: String,
        #[serde(default)]
        is_clustered: bool,
        #[serde(default)]
        instances: Vec<GuestInstance>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestInventorySpecification {
    pub vm_id: ObjectId,
    pub node: NodeName,
    pub lxd: LxdAccess,
    #[serde(with = "time::serde::rfc3339")]
    pub changed_at: OffsetDateTime,
}

impl Validate for GuestInventorySpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        if let LxdAccess::Trusted { instances, .. } = &self.lxd {
            error.check(
                instances.len() <= MAXIMUM_LISTED_INSTANCES,
                "specification.lxd.instances",
                "must list at most 512 instances",
            );
        }
        error.into_result()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GuestInventoryStatus {}
