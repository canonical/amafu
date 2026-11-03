use std::collections::BTreeMap;

use amafu_api::authentication::{TokenIdentity, generate_token};
use amafu_api::identity::Principal;
use amafu_domain::ProjectName;
use amafu_domain::access::Role;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

pub const PROJECTS: [&str; 2] = ["project-a", "project-b"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerCredentials {
    pub certificate_authority: String,
    pub certificate: String,
    pub private_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientTokens {
    pub by_role_and_project: BTreeMap<String, String>,
    pub expired: String,
}

impl ClientTokens {
    #[must_use]
    pub fn key(role: Role, project: &ProjectName) -> String {
        format!("{role}@{project}")
    }

    #[must_use]
    pub fn token(&self, role: Role, project: &ProjectName) -> Option<String> {
        self.by_role_and_project.get(&Self::key(role, project)).cloned()
    }
}

pub fn client_tokens(now: OffsetDateTime) -> anyhow::Result<(ClientTokens, Vec<TokenIdentity>)> {
    let mut by_role_and_project = BTreeMap::new();
    let mut identities = Vec::new();
    for project in PROJECTS {
        let project = ProjectName::new(project)?;
        for role in Role::ALL {
            let token = generate_token();
            let principal =
                Principal { name: format!("{role}-of-{project}"), role, project: project.clone() };
            identities.push(TokenIdentity::for_token(principal, &token, None));
            by_role_and_project.insert(ClientTokens::key(role, &project), token);
        }
    }
    let expired = generate_token();
    let former = Principal {
        name: String::from("former-admin"),
        role: Role::Admin,
        project: ProjectName::new(PROJECTS[0])?,
    };
    identities.push(TokenIdentity::for_token(former, &expired, Some(now - time::Duration::hours(1))));
    Ok((ClientTokens { by_role_and_project, expired }, identities))
}
