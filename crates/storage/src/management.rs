use std::sync::Arc;
use std::time::Duration;

use amafu_interfaces::ActionError;

pub fn management_client(timeout: Duration) -> Result<reqwest::Client, ActionError> {
    management_client_trusting(timeout, None)
}

pub fn management_client_trusting(
    timeout: Duration,
    certificate_authority: Option<&str>,
) -> Result<reqwest::Client, ActionError> {
    let mut roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    if let Some(pem) = certificate_authority {
        for certificate in rustls_pemfile::certs(&mut pem.as_bytes()) {
            roots.add(certificate.map_err(ActionError::invalid)?).map_err(ActionError::invalid)?;
        }
    }
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(ActionError::fatal)?
        .with_root_certificates(roots)
        .with_no_client_auth();
    reqwest::Client::builder().use_preconfigured_tls(tls).timeout(timeout).build().map_err(ActionError::fatal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_management_client_builds_without_a_process_wide_crypto_provider() {
        assert!(management_client(Duration::from_secs(1)).is_ok());
    }

    #[test]
    fn an_unreadable_certificate_authority_is_refused() {
        let broken = "-----BEGIN CERTIFICATE-----\nnot base64\n-----END CERTIFICATE-----\n";
        assert!(management_client_trusting(Duration::from_secs(1), Some(broken)).is_err());
    }
}
