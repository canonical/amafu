use amafu_interfaces::ActionError;
use rcgen::{BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair, KeyUsagePurpose};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateAuthority {
    pub certificate_pem: String,
    pub private_key_pem: String,
}

impl CertificateAuthority {
    pub fn generate(common_name: &str) -> Result<Self, ActionError> {
        let key = KeyPair::generate().map_err(ActionError::fatal)?;
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
