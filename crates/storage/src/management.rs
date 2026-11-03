use std::sync::Arc;
use std::time::Duration;

use amafu_interfaces::ActionError;

pub fn management_client(timeout: Duration) -> Result<reqwest::Client, ActionError> {
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
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
}
