use std::collections::BTreeMap;

use amafu_domain::{ObjectName, ProjectName, Violation};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

pub const REQUEST_ID_HEADER: &str = "x-request-id";
pub const MAXIMUM_BODY_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewObjectMetadata {
    pub name: ObjectName,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRequest {
    pub metadata: NewObjectMetadata,
    pub specification: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_generation: Option<u64>,
    pub specification: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListResponse {
    pub items: Vec<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorCode {
    Unauthenticated,
    Forbidden,
    NotFound,
    AlreadyExists,
    Conflict,
    Malformed,
    Invalid,
    Unavailable,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ErrorCode,
    pub message: String,
    pub request_id: Uuid,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub violations: Vec<Violation>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectName>,
    #[serde(default, rename = "all-projects", skip_serializing_if = "std::ops::Not::not")]
    pub all_projects: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum WatchFrame {
    Object { object: Value },
    Removed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum CollectionFrame {
    Snapshot {
        items: Vec<Value>,
    },
    Changed {
        object: Value,
    },
    Removed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<ProjectName>,
        name: ObjectName,
    },
}
