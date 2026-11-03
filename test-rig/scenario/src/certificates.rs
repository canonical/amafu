use std::net::IpAddr;

use amafu_api::node_identity::{NODE_URI_PREFIX, ROLE_URI_PREFIX};
use amafu_domain::NodeName;
use amafu_domain::kinds::node::NodeRole;
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer,
    KeyPair, KeyUsagePurpose, SanType,
};
use serde::{Deserialize, Serialize};

use crate::error::ScenarioError;

pub const STATE_SERVER_NAME: &str = "amafu-state";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TlsIdentity {
    pub certificate_der: Vec<u8>,
    pub certificate_pem: String,
    pub private_key_pem: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertificateAuthority {
    pub certificate_pem: String,
    pub private_key_pem: String,
}

#[allow(clippy::needless_pass_by_value)]
fn failure(error: rcgen::Error) -> ScenarioError {
    ScenarioError::Lab(format!("issuing a certificate failed: {error}"))
}

fn role_name(role: NodeRole) -> &'static str {
    match role {
        NodeRole::Core => "core",
        NodeRole::Compute => "compute",
    }
}

impl CertificateAuthority {
    pub fn generate(name: &str) -> Result<Self, ScenarioError> {
        let key = KeyPair::generate().map_err(failure)?;
        let mut parameters = CertificateParams::new(Vec::<String>::new()).map_err(failure)?;
        parameters.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        parameters.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let mut distinguished_name = DistinguishedName::new();
        distinguished_name.push(DnType::CommonName, name);
        parameters.distinguished_name = distinguished_name;
        let certificate = parameters.self_signed(&key).map_err(failure)?;
        Ok(Self { certificate_pem: certificate.pem(), private_key_pem: key.serialize_pem() })
    }

    fn issue(&self, parameters: &CertificateParams) -> Result<TlsIdentity, ScenarioError> {
        let authority_key = KeyPair::from_pem(&self.private_key_pem).map_err(failure)?;
        let issuer = Issuer::from_ca_cert_pem(&self.certificate_pem, authority_key).map_err(failure)?;
        let key = KeyPair::generate().map_err(failure)?;
        let certificate = parameters.signed_by(&key, &issuer).map_err(failure)?;
        Ok(TlsIdentity {
            certificate_der: certificate.der().to_vec(),
            certificate_pem: certificate.pem(),
            private_key_pem: key.serialize_pem(),
        })
    }

    pub fn issue_node(&self, node: &NodeName, roles: &[NodeRole]) -> Result<TlsIdentity, ScenarioError> {
        let mut parameters =
            CertificateParams::new(vec![node.to_string(), STATE_SERVER_NAME.to_owned()]).map_err(failure)?;
        let uri =
            |text: String| text.try_into().map(SanType::URI).map_err(|error: rcgen::Error| failure(error));
        parameters.subject_alt_names.push(uri(format!("{NODE_URI_PREFIX}{node}"))?);
        for role in roles {
            parameters.subject_alt_names.push(uri(format!("{ROLE_URI_PREFIX}{}", role_name(*role)))?);
        }
        parameters.extended_key_usages =
            vec![ExtendedKeyUsagePurpose::ClientAuth, ExtendedKeyUsagePurpose::ServerAuth];
        let mut distinguished_name = DistinguishedName::new();
        distinguished_name.push(DnType::CommonName, node.as_str());
        parameters.distinguished_name = distinguished_name;
        self.issue(&parameters)
    }

    pub fn issue_server(&self, name: &str, addresses: &[IpAddr]) -> Result<TlsIdentity, ScenarioError> {
        let mut parameters = CertificateParams::new(vec![name.to_owned()]).map_err(failure)?;
        parameters.subject_alt_names.extend(addresses.iter().copied().map(SanType::IpAddress));
        parameters.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let mut distinguished_name = DistinguishedName::new();
        distinguished_name.push(DnType::CommonName, name);
        parameters.distinguished_name = distinguished_name;
        self.issue(&parameters)
    }
}

#[cfg(test)]
mod tests {
    use amafu_api::node_identity::node_identity_of;

    use super::*;

    #[test]
    fn issued_node_certificates_name_their_node_and_roles() {
        let authority = CertificateAuthority::generate("test").unwrap();
        let node = NodeName::new("node-4").unwrap();
        let identity = authority.issue_node(&node, &[NodeRole::Compute]).unwrap();
        let (name, roles) = node_identity_of(&identity.certificate_der).unwrap();
        assert_eq!(name, node);
        assert_eq!(roles.into_iter().collect::<Vec<_>>(), vec![NodeRole::Compute]);
    }
}
