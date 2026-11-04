use std::sync::Arc;

use amafu_interfaces::{AuditSinkDelivery, SequencedAuditEntry};
use rustls::pki_types::ServerName;
use time::format_description::well_known::Rfc3339;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

const AUTHENTICATION_INFORMATIONAL: u8 = 110;
const APPLICATION: &str = "amafu";
const MESSAGE_IDENTIFIER: &str = "audit";

pub struct SyslogSink {
    address: String,
    tls: Option<(TlsConnector, ServerName<'static>)>,
}

#[must_use]
pub fn framed(entry: &SequencedAuditEntry) -> Vec<u8> {
    let timestamp = entry.entry.recorded_at.format(&Rfc3339).unwrap_or_else(|_| String::from("-"));
    let body = serde_json::to_string(entry).unwrap_or_default();
    let message = format!(
        "<{AUTHENTICATION_INFORMATIONAL}>1 {timestamp} - {APPLICATION} - {MESSAGE_IDENTIFIER} - {body}"
    );
    format!("{} {message}", message.len()).into_bytes()
}

async fn write_all(
    mut stream: impl AsyncWrite + Unpin,
    entries: &[SequencedAuditEntry],
) -> std::io::Result<()> {
    for entry in entries {
        stream.write_all(&framed(entry)).await?;
    }
    stream.flush().await?;
    stream.shutdown().await
}

impl SyslogSink {
    pub fn new(
        address: String,
        server_name: Option<String>,
        certificate_authority: Option<&str>,
    ) -> Result<Self, String> {
        let tls = match (server_name, certificate_authority) {
            (Some(server_name), Some(pem)) => {
                let mut roots = rustls::RootCertStore::empty();
                for certificate in rustls_pemfile::certs(&mut pem.as_bytes()) {
                    roots
                        .add(certificate.map_err(|error| error.to_string())?)
                        .map_err(|error| error.to_string())?;
                }
                let settings = rustls::ClientConfig::builder_with_provider(Arc::new(
                    rustls::crypto::ring::default_provider(),
                ))
                .with_safe_default_protocol_versions()
                .map_err(|error| error.to_string())?
                .with_root_certificates(roots)
                .with_no_client_auth();
                let name = ServerName::try_from(server_name).map_err(|error| error.to_string())?;
                Some((TlsConnector::from(Arc::new(settings)), name))
            }
            _ => None,
        };
        Ok(Self { address, tls })
    }
}

#[async_trait::async_trait]
impl AuditSinkDelivery for SyslogSink {
    async fn deliver(&self, entries: &[SequencedAuditEntry]) -> Result<(), String> {
        let stream = TcpStream::connect(&self.address).await.map_err(|error| error.to_string())?;
        match &self.tls {
            Some((connector, name)) => {
                let secured =
                    connector.connect(name.clone(), stream).await.map_err(|error| error.to_string())?;
                write_all(secured, entries).await
            }
            None => write_all(stream, entries).await,
        }
        .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use amafu_interfaces::{AuditEntry, AuditOutcome};
    use tokio::io::AsyncReadExt;

    use super::*;

    fn entry(sequence: u64) -> SequencedAuditEntry {
        SequencedAuditEntry {
            sequence,
            entry: AuditEntry {
                request_id: uuid::Uuid::from_u128(1),
                recorded_at: time::OffsetDateTime::UNIX_EPOCH,
                principal: Some(String::from("admin")),
                role: None,
                verb: None,
                kind: None,
                project: None,
                name: None,
                method: String::from("GET"),
                path: String::from("/v1/nodes"),
                outcome: AuditOutcome::Denied,
            },
        }
    }

    #[test]
    fn each_entry_is_an_octet_counted_message() {
        let frame = String::from_utf8(framed(&entry(7))).unwrap();
        let (length, message) = frame.split_once(' ').unwrap();
        assert_eq!(length.parse::<usize>().unwrap(), message.len());
        assert!(message.starts_with("<110>1 1970-01-01T00:00:00Z - amafu - audit - {"));
        assert!(message.contains("\"sequence\":7"));
    }

    #[tokio::test]
    async fn entries_reach_a_collector_over_tcp() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let collector = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut received = Vec::new();
            stream.read_to_end(&mut received).await.unwrap();
            received
        });
        let sink = SyslogSink::new(address, None, None).unwrap();
        sink.deliver(&[entry(1), entry(2)]).await.unwrap();
        let received = String::from_utf8(collector.await.unwrap()).unwrap();
        assert!(received.contains("\"sequence\":1") && received.contains("\"sequence\":2"));
    }
}
