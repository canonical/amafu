use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    Admin,
    Operator,
    Member,
    Viewer,
}

impl Role {
    pub const ALL: [Self; 4] = [Self::Admin, Self::Operator, Self::Member, Self::Viewer];
}

impl fmt::Display for Role {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Admin => "admin",
            Self::Operator => "operator",
            Self::Member => "member",
            Self::Viewer => "viewer",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verb {
    Get,
    List,
    Watch,
    Create,
    Update,
    Delete,
}

impl Verb {
    pub const ALL: [Self; 6] = [Self::Get, Self::List, Self::Watch, Self::Create, Self::Update, Self::Delete];

    #[must_use]
    pub fn is_read(self) -> bool {
        matches!(self, Self::Get | Self::List | Self::Watch)
    }
}

impl fmt::Display for Verb {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Get => "get",
            Self::List => "list",
            Self::Watch => "watch",
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
        })
    }
}
