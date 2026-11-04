use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use amafu_domain::kinds::certificate_revocation::CertificateRevocation;
use amafu_domain::kinds::node::NodeRole;
use amafu_domain::{NodeName, Sha256Digest};
use amafu_interfaces::objects::list;
use amafu_interfaces::{Consistency, Store};
use rustls::pki_types::{CertificateDer, UnixTime};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use x509_parser::extensions::GeneralName;

use crate::server::{TlsError, parse_certificates};

pub const NODE_URI_PREFIX: &str = "urn:amafu:node:";
pub const ROLE_URI_PREFIX: &str = "urn:amafu:role:";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeerCertificate(pub Option<Vec<u8>>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeCredentials {
    pub name: NodeName,
    pub roles: BTreeSet<NodeRole>,
    pub fingerprint: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NodeRefusal {
    #[error("no client certificate was presented")]
    NoCertificate,
    #[error("the certificate was not issued by this cluster: {0}")]
    Untrusted(String),
    #[error("the certificate does not name a node")]
    Anonymous,
    #[error("the certificate {0} was revoked")]
    Revoked(Sha256Digest),
    #[error("the revocations cannot be read: {0}")]
    Unavailable(String),
}

#[must_use]
pub fn fingerprint(certificate: &[u8]) -> Sha256Digest {
    hex::encode(Sha256::digest(certificate)).parse().unwrap_or_else(|_| unreachable!())
}

fn verify_chain(
    authority: &CertificateDer<'_>,
    certificate: &[u8],
    now: OffsetDateTime,
) -> Result<(), String> {
    let anchor = webpki::anchor_from_trusted_cert(authority).map_err(|error| error.to_string())?;
    let presented = CertificateDer::from(certificate);
    let end_entity = webpki::EndEntityCert::try_from(&presented).map_err(|error| error.to_string())?;
    let seconds = u64::try_from(now.unix_timestamp()).unwrap_or_default();
    end_entity
        .verify_for_usage(
            webpki::ALL_VERIFICATION_ALGS,
            &[anchor],
            &[],
            UnixTime::since_unix_epoch(Duration::from_secs(seconds)),
            webpki::KeyUsage::client_auth(),
            None,
            None,
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn uniform_resource_names(certificate: &[u8]) -> Result<Vec<String>, NodeRefusal> {
    let (_, parsed) = x509_parser::parse_x509_certificate(certificate)
        .map_err(|error| NodeRefusal::Untrusted(error.to_string()))?;
    let names = parsed
        .subject_alternative_name()
        .map_err(|error| NodeRefusal::Untrusted(error.to_string()))?
        .map(|extension| {
            extension
                .value
                .general_names
                .iter()
                .filter_map(|name| match name {
                    GeneralName::URI(uri) => Some((*uri).to_owned()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(names)
}

fn role_named(name: &str) -> Option<NodeRole> {
    serde_json::from_value(serde_json::Value::String(name.to_owned())).ok()
}

pub fn node_identity_of(certificate: &[u8]) -> Result<(NodeName, BTreeSet<NodeRole>), NodeRefusal> {
    let names = uniform_resource_names(certificate)?;
    let node = names
        .iter()
        .find_map(|name| name.strip_prefix(NODE_URI_PREFIX))
        .and_then(|name| NodeName::new(name).ok())
        .ok_or(NodeRefusal::Anonymous)?;
    let roles =
        names.iter().filter_map(|name| name.strip_prefix(ROLE_URI_PREFIX)).filter_map(role_named).collect();
    Ok((node, roles))
}

pub struct NodeAuthenticator {
    authority: CertificateDer<'static>,
    store: Arc<dyn Store>,
}

impl NodeAuthenticator {
    pub fn new(authority_pem: &[u8], store: Arc<dyn Store>) -> Result<Self, TlsError> {
        let authority =
            parse_certificates(authority_pem)?.into_iter().next().ok_or(TlsError::MissingCertificate)?;
        Ok(Self { authority, store })
    }

    pub async fn authenticate(
        &self,
        peer: &PeerCertificate,
        now: OffsetDateTime,
    ) -> Result<NodeCredentials, NodeRefusal> {
        let certificate = peer.0.as_deref().ok_or(NodeRefusal::NoCertificate)?;
        verify_chain(&self.authority, certificate, now).map_err(NodeRefusal::Untrusted)?;
        let (name, roles) = node_identity_of(certificate)?;
        let fingerprint = fingerprint(certificate);
        let revocations = list::<CertificateRevocation>(self.store.as_ref(), None, Consistency::Stale)
            .await
            .map_err(|error| NodeRefusal::Unavailable(error.to_string()))?;
        if revocations.iter().any(|revocation| revocation.specification.fingerprint == fingerprint) {
            return Err(NodeRefusal::Revoked(fingerprint));
        }
        Ok(NodeCredentials { name, roles, fingerprint })
    }
}

#[cfg(test)]
mod tests {
    use rcgen::{CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, SanType};

    use super::*;

    struct Authority {
        pem: String,
        issuer: Issuer<'static, KeyPair>,
    }

    fn authority(name: &str) -> Authority {
        let mut parameters = CertificateParams::new(Vec::<String>::new()).unwrap();
        parameters.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        parameters.distinguished_name.push(DnType::CommonName, name);
        let key = KeyPair::generate().unwrap();
        let certificate = parameters.self_signed(&key).unwrap();
        Authority { pem: certificate.pem(), issuer: Issuer::new(parameters, key) }
    }

    fn node_certificate(authority: &Authority, uris: &[&str]) -> Vec<u8> {
        let mut parameters = CertificateParams::new(Vec::<String>::new()).unwrap();
        parameters.subject_alt_names =
            uris.iter().map(|uri| SanType::URI((*uri).try_into().unwrap())).collect();
        parameters.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        let key = KeyPair::generate().unwrap();
        parameters.signed_by(&key, &authority.issuer).unwrap().der().to_vec()
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    #[test]
    fn a_certificate_names_its_node_and_roles() {
        let cluster = authority("cluster");
        let certificate = node_certificate(
            &cluster,
            &["urn:amafu:node:node-3", "urn:amafu:role:compute", "urn:amafu:role:bogus"],
        );
        let (name, roles) = node_identity_of(&certificate).unwrap();
        assert_eq!(name.as_str(), "node-3");
        assert_eq!(roles, BTreeSet::from([NodeRole::Compute]));
        assert_eq!(node_identity_of(&node_certificate(&cluster, &[])), Err(NodeRefusal::Anonymous));
    }

    #[test]
    fn only_certificates_from_the_cluster_authority_are_trusted() {
        let cluster = authority("cluster");
        let foreign = authority("foreign");
        let anchor = parse_certificates(cluster.pem.as_bytes()).unwrap().remove(0);
        let ours = node_certificate(&cluster, &["urn:amafu:node:node-1"]);
        let theirs = node_certificate(&foreign, &["urn:amafu:node:node-1"]);
        assert_eq!(verify_chain(&anchor, &ours, now()), Ok(()));
        assert!(verify_chain(&anchor, &theirs, now()).is_err());
        assert!(verify_chain(&anchor, b"garbage", now()).is_err());
    }

    #[test]
    fn fingerprints_are_the_digest_of_the_certificate() {
        assert_eq!(
            fingerprint(b"").hexadecimal(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
