use std::io::BufReader;
use std::sync::Arc;

use axum::Router;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::service::TowerToHyperService;
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("the certificate or key cannot be read: {0}")]
    Read(#[from] std::io::Error),
    #[error("no private key was found")]
    MissingPrivateKey,
    #[error("no certificate was found")]
    MissingCertificate,
    #[error("the TLS configuration is invalid: {0}")]
    Invalid(#[from] rustls::Error),
}

pub fn parse_certificates(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, TlsError> {
    let certificates = rustls_pemfile::certs(&mut BufReader::new(pem))
        .collect::<Result<Vec<CertificateDer<'static>>, _>>()?;
    if certificates.is_empty() {
        return Err(TlsError::MissingCertificate);
    }
    Ok(certificates)
}

fn parse_private_key(pem: &[u8]) -> Result<PrivateKeyDer<'static>, TlsError> {
    rustls_pemfile::private_key(&mut BufReader::new(pem))?.ok_or(TlsError::MissingPrivateKey)
}

#[must_use]
pub fn crypto_provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

pub fn tls_configuration(
    certificate_chain_pem: &[u8],
    private_key_pem: &[u8],
) -> Result<Arc<ServerConfig>, TlsError> {
    let mut configuration = ServerConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(parse_certificates(certificate_chain_pem)?, parse_private_key(private_key_pem)?)?;
    configuration.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(configuration))
}

pub async fn serve(listener: TcpListener, tls: Option<Arc<ServerConfig>>, router: Router) -> std::io::Error {
    let acceptor = tls.map(TlsAcceptor::from);
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                tracing::warn!(%error, "accepting a connection failed");
                continue;
            }
        };
        let service = TowerToHyperService::new(router.clone());
        let acceptor = acceptor.clone();
        tokio::spawn(async move {
            let builder = auto::Builder::new(TokioExecutor::new());
            let result = match acceptor {
                Some(acceptor) => match acceptor.accept(stream).await {
                    Ok(stream) => builder.serve_connection_with_upgrades(TokioIo::new(stream), service).await,
                    Err(error) => {
                        tracing::debug!(%peer, %error, "TLS handshake failed");
                        return;
                    }
                },
                None => builder.serve_connection_with_upgrades(TokioIo::new(stream), service).await,
            };
            if let Err(error) = result {
                tracing::debug!(%peer, %error, "connection ended with an error");
            }
        });
    }
}
