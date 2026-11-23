use std::sync::Arc;

use amafu_interfaces::ActionError;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

pub fn trusting(certificate_authority_pem: &str) -> Result<rustls::ClientConfig, ActionError> {
    let mut roots = rustls::RootCertStore::empty();
    for certificate in rustls_pemfile::certs(&mut certificate_authority_pem.as_bytes()) {
        roots.add(certificate.map_err(ActionError::fatal)?).map_err(ActionError::fatal)?;
    }
    Ok(rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(ActionError::fatal)?
        .with_root_certificates(roots)
        .with_no_client_auth())
}

fn any_server()
-> Result<rustls::ConfigBuilder<rustls::ClientConfig, rustls::client::WantsClientCert>, ActionError> {
    let provider = provider();
    Ok(rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .map_err(ActionError::fatal)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AnyServer { provider })))
}

pub fn self_signed() -> Result<rustls::ClientConfig, ActionError> {
    Ok(any_server()?.with_no_client_auth())
}

pub fn self_signed_as(
    certificate_pem: &str,
    private_key_pem: &str,
) -> Result<rustls::ClientConfig, ActionError> {
    let certificates: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut certificate_pem.as_bytes())
        .collect::<Result<_, _>>()
        .map_err(ActionError::fatal)?;
    let key = rustls_pemfile::private_key(&mut private_key_pem.as_bytes())
        .map_err(ActionError::fatal)?
        .ok_or_else(|| ActionError::fatal("the client identity holds no private key"))?;
    any_server()?.with_client_auth_cert(certificates, key).map_err(ActionError::fatal)
}

#[derive(Debug)]
struct AnyServer {
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for AnyServer {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            certificate,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            certificate,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}
