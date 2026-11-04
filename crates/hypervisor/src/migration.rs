use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use amafu_domain::kinds::vm::MigrationReport;
use amafu_domain::{NodeName, ObjectId};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::server::WebPkiClientVerifier;
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream, UnixStream};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::runtime::{RuntimeDirectory, VmDirectory};

pub const MIGRATION_PORT: u16 = 7445;
const HEADER_PREFIX: &str = "amafu-migration/1 ";
const LONGEST_HEADER: u64 = 128;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("the migration identity is unusable: {0}")]
    Identity(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum MigrationRecord {
    Sending { attempt: ObjectId },
    Sent { attempt: ObjectId, report: MigrationReport },
    Failed { attempt: ObjectId, reason: String },
}

impl MigrationRecord {
    #[must_use]
    pub fn attempt(&self) -> ObjectId {
        match self {
            Self::Sending { attempt } | Self::Sent { attempt, .. } | Self::Failed { attempt, .. } => *attempt,
        }
    }

    #[must_use]
    pub fn read(directory: &VmDirectory) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(directory.migration_file()).ok()?).ok()
    }

    pub fn write(&self, directory: &VmDirectory) -> io::Result<()> {
        std::fs::write(directory.migration_file(), serde_json::to_vec(self).map_err(io::Error::other)?)
    }
}

pub struct MigrationTransport {
    acceptor: TlsAcceptor,
    connector: TlsConnector,
}

fn certificates(pem: &str) -> Result<Vec<CertificateDer<'static>>, TransportError> {
    rustls_pemfile::certs(&mut pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| TransportError::Identity(error.to_string()))
}

fn private_key(pem: &str) -> Result<PrivateKeyDer<'static>, TransportError> {
    rustls_pemfile::private_key(&mut pem.as_bytes())
        .map_err(|error| TransportError::Identity(error.to_string()))?
        .ok_or_else(|| TransportError::Identity(String::from("no private key was given")))
}

fn header(vm_id: &ObjectId) -> String {
    format!("{HEADER_PREFIX}{vm_id}\n")
}

fn parse_header(line: &str) -> Option<ObjectId> {
    line.strip_prefix(HEADER_PREFIX)?.trim_end().parse().ok()
}

impl MigrationTransport {
    pub fn new(
        certificate_pem: &str,
        private_key_pem: &str,
        certificate_authority_pem: &str,
    ) -> Result<Self, TransportError> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut roots = RootCertStore::empty();
        for certificate in certificates(certificate_authority_pem)? {
            roots.add(certificate).map_err(|error| TransportError::Identity(error.to_string()))?;
        }
        let roots = Arc::new(roots);
        let identity = certificates(certificate_pem)?;
        let verifier = WebPkiClientVerifier::builder_with_provider(Arc::clone(&roots), Arc::clone(&provider))
            .build()
            .map_err(|error| TransportError::Identity(error.to_string()))?;
        let server = ServerConfig::builder_with_provider(Arc::clone(&provider))
            .with_safe_default_protocol_versions()
            .map_err(|error| TransportError::Identity(error.to_string()))?
            .with_client_cert_verifier(verifier)
            .with_single_cert(identity.clone(), private_key(private_key_pem)?)
            .map_err(|error| TransportError::Identity(error.to_string()))?;
        let client = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|error| TransportError::Identity(error.to_string()))?
            .with_root_certificates(roots)
            .with_client_auth_cert(identity, private_key(private_key_pem)?)
            .map_err(|error| TransportError::Identity(error.to_string()))?;
        Ok(Self {
            acceptor: TlsAcceptor::from(Arc::new(server)),
            connector: TlsConnector::from(Arc::new(client)),
        })
    }

    pub async fn serve(self: Arc<Self>, listener: TcpListener, runtime: RuntimeDirectory) -> io::Error {
        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(accepted) => accepted,
                Err(error) => return error,
            };
            let (transport, runtime) = (Arc::clone(&self), runtime.clone());
            tokio::spawn(async move {
                match transport.deliver(stream, &runtime).await {
                    Ok(bytes) => tracing::info!(%peer, bytes, "an incoming VM arrived"),
                    Err(error) => tracing::warn!(%peer, %error, "an incoming migration failed"),
                }
            });
        }
    }

    async fn deliver(&self, stream: TcpStream, runtime: &RuntimeDirectory) -> io::Result<u64> {
        let tls = tokio::time::timeout(HANDSHAKE_TIMEOUT, self.acceptor.accept(stream))
            .await
            .map_err(|_| io::Error::other("the migration handshake timed out"))??;
        let mut reader = BufReader::new(tls);
        let mut line = String::new();
        (&mut reader).take(LONGEST_HEADER).read_line(&mut line).await?;
        let vm_id =
            parse_header(&line).ok_or_else(|| io::Error::other("the migration header is malformed"))?;
        let directory = runtime.vm(&vm_id);
        if !directory.is_receiving() {
            return Err(io::Error::other(format!("no VM {vm_id} is waiting to arrive here")));
        }
        let mut local = UnixStream::connect(directory.incoming_socket()).await?;
        let (sent, _) = tokio::io::copy_bidirectional(&mut reader, &mut local).await?;
        Ok(sent)
    }

    pub async fn relay(
        &self,
        mut from: UnixStream,
        destination: SocketAddr,
        node: &NodeName,
        vm_id: &ObjectId,
    ) -> io::Result<u64> {
        let stream = TcpStream::connect(destination).await?;
        stream.set_nodelay(true)?;
        let name = ServerName::try_from(node.to_string()).map_err(io::Error::other)?;
        let mut tls = tokio::time::timeout(HANDSHAKE_TIMEOUT, self.connector.connect(name, stream))
            .await
            .map_err(|_| io::Error::other("the migration handshake timed out"))??;
        tls.write_all(header(vm_id).as_bytes()).await?;
        let (sent, _) = tokio::io::copy_bidirectional(&mut from, &mut tls).await?;
        Ok(sent)
    }
}

#[must_use]
pub fn report_from_log(log: &str, relayed_bytes: u64) -> MigrationReport {
    let mut report = MigrationReport { total_bytes: relayed_bytes, ..MigrationReport::default() };
    for message in log.lines().filter_map(|line| {
        let event: serde_json::Value = serde_json::from_str(line).ok()?;
        event["fields"]["message"].as_str().map(str::to_owned)
    }) {
        if let Some(precopy) = message.strip_prefix("Precopy complete: ") {
            report.final_round_bytes = mebibytes_field(precopy, "curr=").unwrap_or_default() * 1024 * 1024;
        }
        if let Some(completed) = message.strip_prefix("Migration completed after ") {
            report.duration_milliseconds = completed
                .split('s')
                .next()
                .and_then(|seconds| seconds.parse::<f64>().ok())
                .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
                .map_or(0, |duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX));
            report.downtime_milliseconds = completed
                .split("downtime of ")
                .nth(1)
                .and_then(|rest| rest.split("ms").next())
                .and_then(|milliseconds| milliseconds.parse().ok())
                .unwrap_or_default();
        }
    }
    report
}

fn mebibytes_field(text: &str, key: &str) -> Option<u64> {
    text.split_whitespace().find_map(|field| field.strip_prefix(key)?.strip_suffix("MiB")?.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_names_the_vm() {
        let vm_id = ObjectId::from_uuid(uuid::Uuid::from_u128(7));
        assert_eq!(parse_header(&header(&vm_id)), Some(vm_id));
        assert_eq!(parse_header("GET / HTTP/1.1\n"), None);
    }

    #[test]
    fn the_report_is_read_from_the_vmm_log() {
        let log = [
            r#"{"level":"INFO","fields":{"message":"Precopy complete: iter=5 curr=3MiB total=4100MiB bw=110.00MiB/s transfer=0.03s overhead=1ms est_downtime=27ms elapsed=38.10s avg_bw=107.61MiB/s"}}"#,
            r#"{"level":"INFO","fields":{"message":"Migration completed after 38.4s with a downtime of 61ms (goal was 100ms)"}}"#,
        ]
        .join("\n");
        let report = report_from_log(&log, 4_300_000_000);
        assert_eq!(report.final_round_bytes, 3 * 1024 * 1024);
        assert_eq!(report.downtime_milliseconds, 61);
        assert_eq!(report.duration_milliseconds, 38_400);
        assert_eq!(report.total_bytes, 4_300_000_000);
    }
}
