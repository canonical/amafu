use amafu_interfaces::ActionError;
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair, KeyUsagePurpose,
    PKCS_RSA_SHA256,
};
use rsa::RsaPrivateKey;
use rsa::pkcs8::{EncodePrivateKey, LineEnding};
use rsa::rand_core::OsRng;

const RSA_BITS: usize = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorityKey {
    Ecdsa,
    Rsa,
}

impl AuthorityKey {
    fn generate(self) -> Result<KeyPair, ActionError> {
        match self {
            Self::Ecdsa => KeyPair::generate().map_err(ActionError::fatal),
            Self::Rsa => {
                let key = RsaPrivateKey::new(&mut OsRng, RSA_BITS).map_err(ActionError::fatal)?;
                let pem = key.to_pkcs8_pem(LineEnding::LF).map_err(ActionError::fatal)?;
                KeyPair::from_pkcs8_pem_and_sign_algo(&pem, &PKCS_RSA_SHA256).map_err(ActionError::fatal)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateAuthority {
    pub certificate_pem: String,
    pub private_key_pem: String,
}

impl CertificateAuthority {
    pub fn generate(common_name: &str, key: AuthorityKey) -> Result<Self, ActionError> {
        let key = key.generate()?;
        let mut parameters = CertificateParams::new(Vec::<String>::new()).map_err(ActionError::fatal)?;
        parameters.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        parameters.key_usages =
            vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign, KeyUsagePurpose::DigitalSignature];
        let mut distinguished_name = DistinguishedName::new();
        distinguished_name.push(DnType::CommonName, common_name);
        parameters.distinguished_name = distinguished_name;
        let certificate = parameters.self_signed(&key).map_err(ActionError::fatal)?;
        Ok(Self { certificate_pem: certificate.pem(), private_key_pem: key.serialize_pem() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_rsa_authority_is_a_self_signed_rsa_ca() {
        let authority = CertificateAuthority::generate("cos Kubernetes", AuthorityKey::Rsa).unwrap();
        assert!(authority.private_key_pem.starts_with("-----BEGIN PRIVATE KEY-----"));
        let (_, pem) = x509_parser::pem::parse_x509_pem(authority.certificate_pem.as_bytes()).unwrap();
        let certificate = pem.parse_x509().unwrap();
        assert_eq!(
            certificate.public_key().algorithm.algorithm,
            x509_parser::oid_registry::OID_PKCS1_RSAENCRYPTION
        );
        assert!(certificate.is_ca());
    }
}
