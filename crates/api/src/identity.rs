use amafu_domain::ProjectName;
use amafu_domain::access::Role;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Principal {
    pub name: String,
    pub role: Role,
    pub project: ProjectName,
}
