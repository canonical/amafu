use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::access::Role;
use crate::resource::{Resource, Scope};
use crate::validation::{Validate, ValidationError};

pub const MAXIMUM_FEDERATED_IDENTITIES: usize = 16;
pub const MAXIMUM_SECRETS: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceAccount;

impl Resource for ServiceAccount {
    const KIND: &'static str = "ServiceAccount";
    const PLURAL_NAME: &'static str = "service-accounts";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Project;
    type Specification = ServiceAccountSpecification;
    type Status = ServiceAccountStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FederatedIdentity {
    pub issuer: String,
    pub subject: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceAccountSpecification {
    pub role: Role,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub federated_identities: Vec<FederatedIdentity>,
}

impl Validate for ServiceAccountSpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(self.role != Role::Admin, "specification.role", "must be viewer, member or operator");
        error.check(
            self.federated_identities.len() <= MAXIMUM_FEDERATED_IDENTITIES,
            "specification.federated_identities",
            "must be at most 16",
        );
        for (index, identity) in self.federated_identities.iter().enumerate() {
            error.check(
                identity.issuer.starts_with("https://"),
                &format!("specification.federated_identities.{index}.issuer"),
                "must be an https URL",
            );
            error.check(
                !identity.subject.is_empty(),
                &format!("specification.federated_identities.{index}.subject"),
                "must not be empty",
            );
        }
        error.into_result()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientSecret {
    pub id: String,
    pub hash: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ServiceAccountStatus {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<ClientSecret>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(role: Role, issuer: &str) -> ServiceAccountSpecification {
        ServiceAccountSpecification {
            role,
            federated_identities: vec![FederatedIdentity {
                issuer: issuer.to_owned(),
                subject: String::from("repo:example/platform:ref:refs/heads/main"),
                audience: None,
            }],
        }
    }

    #[test]
    fn a_service_account_is_never_an_admin_and_trusts_only_https_issuers() {
        assert!(account(Role::Member, "https://token.actions.example").validate().is_ok());
        assert!(account(Role::Admin, "https://token.actions.example").validate().is_err());
        assert!(account(Role::Member, "http://token.actions.example").validate().is_err());
    }
}
