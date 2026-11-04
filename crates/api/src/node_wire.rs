use amafu_domain::kinds::node_lease::LeaseTerms;
use amafu_domain::{ObjectReference, ProjectName};
use amafu_interfaces::StoredRecord;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const NODE_READ_PATH: &str = "/v1/node/read";
pub const NODE_LIST_PATH: &str = "/v1/node/list";
pub const NODE_SWAP_PATH: &str = "/v1/node/compare-and-swap";
pub const NODE_SUBSCRIPTION_PATH: &str = "/v1/node/subscription";
pub const NODE_LEASE_ACQUIRE_PATH: &str = "/v1/node/lease/acquire";
pub const NODE_LEASE_RENEW_PATH: &str = "/v1/node/lease/renew";
pub const NODE_LEASE_SURRENDER_PATH: &str = "/v1/node/lease/surrender";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeReadRequest {
    pub reference: ObjectReference,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeReadResponse {
    pub record: Option<StoredRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeListRequest {
    pub kind: String,
    #[serde(default)]
    pub project: Option<ProjectName>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeListResponse {
    pub records: Vec<StoredRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeSwapRequest {
    pub reference: ObjectReference,
    pub expected_revision: Option<u64>,
    pub replacement: Option<Value>,
    pub epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeSwapResponse {
    pub revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeLeaseAcquireRequest {
    pub terms: LeaseTerms,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeLeaseRenewRequest {
    pub epoch: u64,
}
