use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use amafu_interfaces::{Installation, MachineName, MachineProvisioner, MachineState, ProvisioningError};
use base64::Engine;
use serde::Deserialize;

const API_PATH: &str = "/api/2.0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaasCredentials {
    consumer_key: String,
    token_key: String,
    token_secret: String,
}

impl MaasCredentials {
    pub fn parse(api_key: &str) -> Result<Self, ProvisioningError> {
        let parts: Vec<&str> = api_key.trim().split(':').collect();
        match parts.as_slice() {
            [consumer_key, token_key, token_secret] => Ok(Self {
                consumer_key: (*consumer_key).to_owned(),
                token_key: (*token_key).to_owned(),
                token_secret: (*token_secret).to_owned(),
            }),
            _ => Err(ProvisioningError::Refused(String::from(
                "a MAAS API key has three parts joined by colons",
            ))),
        }
    }

    #[must_use]
    pub fn authorization(&self, nonce: &str, timestamp: u64) -> String {
        format!(
            "OAuth oauth_version=\"1.0\", oauth_signature_method=\"PLAINTEXT\", oauth_consumer_key=\"{}\", oauth_token=\"{}\", oauth_signature=\"&{}\", oauth_nonce=\"{nonce}\", oauth_timestamp=\"{timestamp}\"",
            self.consumer_key, self.token_key, self.token_secret
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MaasMachine {
    pub system_id: String,
    pub hostname: String,
    pub status_name: String,
    #[serde(default)]
    pub status_message: Option<String>,
}

impl MaasMachine {
    #[must_use]
    pub fn state(&self) -> MachineState {
        match self.status_name.as_str() {
            "Ready" => MachineState::Ready,
            "Allocated" => MachineState::Allocated,
            "Deploying" => MachineState::Deploying,
            "Deployed" => MachineState::Deployed,
            failed if failed.starts_with("Failed") || failed == "Broken" => MachineState::Failed {
                reason: format!("{failed}: {}", self.status_message.as_deref().unwrap_or("no message")),
            },
            other => MachineState::Unavailable { status: other.to_owned() },
        }
    }
}

#[must_use]
pub fn form_body(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(key, value)| format!("{}={}", encode(key), encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(byte).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

pub struct MaasClient {
    http: reqwest::Client,
    url: String,
    credentials: MaasCredentials,
}

fn unreachable_maas(error: impl std::fmt::Display) -> ProvisioningError {
    ProvisioningError::Unreachable(error.to_string())
}

impl MaasClient {
    pub fn new(
        url: &str,
        api_key: &str,
        certificate_authority: Option<&str>,
    ) -> Result<Self, ProvisioningError> {
        let mut roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
        if let Some(pem) = certificate_authority {
            for certificate in rustls::pki_types::pem::PemObject::pem_slice_iter(pem.as_bytes()) {
                let certificate: rustls::pki_types::CertificateDer<'static> =
                    certificate.map_err(|error| ProvisioningError::Refused(error.to_string()))?;
                roots.add(certificate).map_err(|error| ProvisioningError::Refused(error.to_string()))?;
            }
        }
        let tls =
            rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .map_err(|error| ProvisioningError::Refused(error.to_string()))?
                .with_root_certificates(roots)
                .with_no_client_auth();
        let http = reqwest::Client::builder()
            .use_preconfigured_tls(tls)
            .build()
            .map_err(|error| ProvisioningError::Refused(error.to_string()))?;
        Ok(Self {
            http,
            url: url.trim_end_matches('/').to_owned(),
            credentials: MaasCredentials::parse(api_key)?,
        })
    }

    fn authorization(&self) -> String {
        let timestamp =
            SystemTime::now().duration_since(UNIX_EPOCH).map(|elapsed| elapsed.as_secs()).unwrap_or_default();
        self.credentials.authorization(&uuid::Uuid::new_v4().simple().to_string(), timestamp)
    }

    async fn answer(&self, request: reqwest::RequestBuilder) -> Result<String, ProvisioningError> {
        let response = request
            .header(reqwest::header::AUTHORIZATION, self.authorization())
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(unreachable_maas)?;
        let status = response.status();
        let body = response.text().await.map_err(unreachable_maas)?;
        if status.is_server_error() {
            return Err(ProvisioningError::Unreachable(format!("MAAS answered {status}: {body}")));
        }
        if !status.is_success() {
            return Err(ProvisioningError::Refused(format!("MAAS answered {status}: {body}")));
        }
        Ok(body)
    }

    async fn post(&self, path: &str, fields: &[(&str, &str)]) -> Result<String, ProvisioningError> {
        let request = self
            .http
            .post(format!("{}{API_PATH}{path}", self.url))
            .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(form_body(fields));
        self.answer(request).await
    }

    pub async fn find(&self, machine: &MachineName) -> Result<MaasMachine, ProvisioningError> {
        let path = format!("{}{API_PATH}/machines/?hostname={}", self.url, encode(&machine.0));
        let body = self.answer(self.http.get(path)).await?;
        let machines: Vec<MaasMachine> =
            serde_json::from_str(&body).map_err(|error| ProvisioningError::Refused(error.to_string()))?;
        machines
            .into_iter()
            .find(|candidate| candidate.hostname == machine.0)
            .ok_or_else(|| ProvisioningError::UnknownMachine(machine.clone()))
    }
}

#[async_trait::async_trait]
impl MachineProvisioner for MaasClient {
    async fn state(&self, machine: &MachineName) -> Result<MachineState, ProvisioningError> {
        Ok(self.find(machine).await?.state())
    }

    async fn deploy(
        &self,
        machine: &MachineName,
        installation: &Installation,
    ) -> Result<(), ProvisioningError> {
        let found = self.find(machine).await?;
        if found.state() == MachineState::Ready {
            self.post("/machines/?op=allocate", &[("system_id", &found.system_id)]).await?;
        }
        let user_data = base64::engine::general_purpose::STANDARD.encode(&installation.user_data);
        self.post(
            &format!("/machines/{}/?op=deploy", found.system_id),
            &[("user_data", &user_data), ("ephemeral_deploy", "true")],
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_signed_with_the_plaintext_oauth_method() {
        let credentials = MaasCredentials::parse("consumer:token:secret").unwrap();
        let header = credentials.authorization("abc", 1_700_000_000);
        assert!(header.starts_with("OAuth oauth_version=\"1.0\", oauth_signature_method=\"PLAINTEXT\""));
        assert!(header.contains("oauth_consumer_key=\"consumer\""));
        assert!(header.contains("oauth_token=\"token\""));
        assert!(header.contains("oauth_signature=\"&secret\""));
        assert!(header.contains("oauth_timestamp=\"1700000000\""));
        assert!(MaasCredentials::parse("secret").is_err());
    }

    #[test]
    fn machine_states_follow_the_maas_status() {
        let machine = |status: &str| MaasMachine {
            system_id: String::from("abc123"),
            hostname: String::from("amafu-0"),
            status_name: status.to_owned(),
            status_message: Some(String::from("curtin failed")),
        };
        assert_eq!(machine("Ready").state(), MachineState::Ready);
        assert_eq!(machine("Deployed").state(), MachineState::Deployed);
        assert!(
            matches!(machine("Failed deployment").state(), MachineState::Failed { reason } if reason.contains("curtin failed"))
        );
        assert_eq!(
            machine("Commissioning").state(),
            MachineState::Unavailable { status: String::from("Commissioning") }
        );
    }

    #[test]
    fn form_values_are_percent_encoded() {
        assert_eq!(
            form_body(&[("user_data", "a+b=c/"), ("system_id", "x y")]),
            "user_data=a%2Bb%3Dc%2F&system_id=x%20y"
        );
    }
}
