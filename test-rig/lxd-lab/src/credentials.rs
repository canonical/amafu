use std::collections::BTreeMap;
use std::net::IpAddr;

use amafu_api::authentication::{TokenIdentity, generate_token};
use amafu_api::identity::Principal;
use amafu_domain::ProjectName;
use amafu_domain::access::Role;
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer,
    KeyPair, KeyUsagePurpose, SanType,
};
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

pub fn server_credentials(node_name: &str, addresses: &[IpAddr]) -> anyhow::Result<ServerCredentials> {
    let authority_key = KeyPair::generate()?;
    let mut authority = CertificateParams::new(Vec::<String>::new())?;
    authority.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    authority.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let mut authority_name = DistinguishedName::new();
    authority_name.push(DnType::CommonName, "Amafu lab certificate authority");
    authority.distinguished_name = authority_name;
    let authority_certificate = authority.self_signed(&authority_key)?;

    let server_key = KeyPair::generate()?;
    let mut server = CertificateParams::new(vec![node_name.to_owned()])?;
    server.subject_alt_names.extend(addresses.iter().copied().map(SanType::IpAddress));
    server.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let mut server_name = DistinguishedName::new();
    server_name.push(DnType::CommonName, node_name);
    server.distinguished_name = server_name;
    let issuer = Issuer::from_params(&authority, &authority_key);
    let server_certificate = server.signed_by(&server_key, &issuer)?;

    Ok(ServerCredentials {
        certificate_authority: authority_certificate.pem(),
        certificate: server_certificate.pem(),
        private_key: server_key.serialize_pem(),
    })
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
