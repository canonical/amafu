use std::sync::Arc;
use std::time::Duration;

#[must_use]
pub fn https_client(timeout: Duration) -> Option<reqwest::Client> {
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .ok()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    reqwest::Client::builder().use_preconfigured_tls(tls).https_only(true).timeout(timeout).build().ok()
}
