use std::io::BufReader;
use std::sync::Arc;

use axum::{Extension, Router};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::service::TowerToHyperService;
use rustls::client::danger::HandshakeSignatureValid;
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, ServerConfig, SignatureScheme};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use crate::node_identity::PeerCertificate;

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

#[derive(Debug)]
struct CertificatesCheckedByTheApplication {
    provider: Arc<CryptoProvider>,
}

impl ClientCertVerifier for CertificatesCheckedByTheApplication {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        false
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
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

pub fn tls_configuration(
    certificate_chain_pem: &[u8],
    private_key_pem: &[u8],
) -> Result<Arc<ServerConfig>, TlsError> {
    let provider = crypto_provider();
    let verifier = Arc::new(CertificatesCheckedByTheApplication { provider: Arc::clone(&provider) });
    let mut configuration = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_client_cert_verifier(verifier)
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
        let router = router.clone();
        let acceptor = acceptor.clone();
        tokio::spawn(async move {
            let result = match acceptor {
                Some(acceptor) => serve_tls_connection(&acceptor, stream, router).await,
                None => serve_plain_connection(stream, router).await,
            };
            if let Err(error) = result {
                tracing::debug!(%peer, %error, "connection ended with an error");
            }
        });
    }
}

type ConnectionResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

async fn serve_tls_connection(
    acceptor: &TlsAcceptor,
    stream: tokio::net::TcpStream,
    router: Router,
) -> ConnectionResult {
    let stream = acceptor.accept(stream).await?;
    let presented = stream
        .get_ref()
        .1
        .peer_certificates()
        .and_then(<[CertificateDer<'_>]>::first)
        .map(|certificate| certificate.to_vec());
    let service = TowerToHyperService::new(router.layer(Extension(PeerCertificate(presented))));
    auto::Builder::new(TokioExecutor::new())
        .serve_connection_with_upgrades(TokioIo::new(stream), service)
        .await
}

async fn serve_plain_connection(stream: tokio::net::TcpStream, router: Router) -> ConnectionResult {
    let service = TowerToHyperService::new(router.layer(Extension(PeerCertificate::default())));
    auto::Builder::new(TokioExecutor::new())
        .serve_connection_with_upgrades(TokioIo::new(stream), service)
        .await
}
