use amafu_interfaces::{AuditSinkDelivery, SequencedAuditEntry};
use time::OffsetDateTime;
use time::macros::format_description;

use crate::signature::{Credentials, SignedRequest, authorization, digest_of};

pub struct ObjectStorageSink {
    http: reqwest::Client,
    endpoint: String,
    bucket: String,
    region: String,
    prefix: String,
    access_key: String,
    secret_key: String,
}

fn host_of(endpoint: &str) -> &str {
    endpoint.split("://").nth(1).unwrap_or(endpoint).split('/').next().unwrap_or(endpoint)
}

#[must_use]
pub fn object_name(prefix: &str, first: u64, last: u64) -> String {
    let separator = if prefix.is_empty() || prefix.ends_with('/') { "" } else { "/" };
    format!("{prefix}{separator}audit-{first:020}-{last:020}.ndjson")
}

#[must_use]
pub fn newline_delimited(entries: &[SequencedAuditEntry]) -> Vec<u8> {
    let mut body = Vec::new();
    for entry in entries {
        if let Ok(line) = serde_json::to_vec(entry) {
            body.extend(line);
            body.push(b'\n');
        }
    }
    body
}

impl ObjectStorageSink {
    pub fn new(
        endpoint: &str,
        bucket: &str,
        region: &str,
        prefix: &str,
        access_key: &str,
        secret_key: &str,
    ) -> Result<Self, String> {
        let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
        let tls = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|error| error.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
        let http = reqwest::Client::builder()
            .use_preconfigured_tls(tls)
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            http,
            endpoint: endpoint.trim_end_matches('/').to_owned(),
            bucket: bucket.to_owned(),
            region: region.to_owned(),
            prefix: prefix.to_owned(),
            access_key: access_key.to_owned(),
            secret_key: secret_key.to_owned(),
        })
    }
}

#[async_trait::async_trait]
impl AuditSinkDelivery for ObjectStorageSink {
    async fn deliver(&self, entries: &[SequencedAuditEntry]) -> Result<(), String> {
        let (Some(first), Some(last)) = (entries.first(), entries.last()) else {
            return Ok(());
        };
        let body = newline_delimited(entries);
        let body_digest = digest_of(&body);
        let path = format!("/{}/{}", self.bucket, object_name(&self.prefix, first.sequence, last.sequence));
        let timestamp = OffsetDateTime::now_utc()
            .format(format_description!("[year][month][day]T[hour][minute][second]Z"))
            .map_err(|error| error.to_string())?;
        let headers = vec![
            (String::from("host"), host_of(&self.endpoint).to_owned()),
            (String::from("x-amz-content-sha256"), body_digest.clone()),
            (String::from("x-amz-date"), timestamp.clone()),
        ];
        let signed =
            SignedRequest { method: "PUT", path: &path, headers: headers.clone(), body_digest: &body_digest };
        let credentials =
            Credentials { access_key: &self.access_key, secret_key: &self.secret_key, region: &self.region };
        let mut request = self
            .http
            .put(format!("{}{path}", self.endpoint))
            .header(reqwest::header::AUTHORIZATION, authorization(&signed, &credentials, &timestamp))
            .body(body);
        for (name, value) in headers.iter().filter(|(name, _)| name != "host") {
            request = request.header(name, value);
        }
        let response = request.send().await.map_err(|error| error.to_string())?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(format!("the object store answered {}", response.status()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn objects_sort_by_the_entries_they_hold() {
        assert_eq!(object_name("", 1, 2), "audit-00000000000000000001-00000000000000000002.ndjson");
        assert_eq!(
            object_name("cluster-a", 1, 2),
            "cluster-a/audit-00000000000000000001-00000000000000000002.ndjson"
        );
        assert!(object_name("x/", 9, 10) < object_name("x/", 10, 11));
        assert_eq!(host_of("https://objects.example:9000/base"), "objects.example:9000");
    }
}
