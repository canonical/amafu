use std::sync::Arc;
use std::time::Duration;

use amafu_interfaces::juju::JUJU_API_PORT;
use amafu_interfaces::{ActionError, JujuCredentials, JujuEndpoint};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use futures::{SinkExt, StreamExt};
use rustls::pki_types::ServerName;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

use crate::facades::{Facades, LoginResult};

const SERVER_NAME: &str = "juju-apiserver";
const CLIENT_VERSION: &str = "4.0.0";
const ADMIN_VERSION: u32 = 3;

pub enum Scope<'a> {
    Controller,
    Model(&'a str),
}

pub struct Connection {
    socket: WebSocketStream<TlsStream<TcpStream>>,
    next_request: u64,
    facades: Facades,
    timeout: Duration,
}

fn tls(certificate_authority: &str) -> Result<rustls::ClientConfig, ActionError> {
    let mut roots = rustls::RootCertStore::empty();
    for certificate in rustls_pemfile::certs(&mut certificate_authority.as_bytes()) {
        roots.add(certificate.map_err(ActionError::fatal)?).map_err(ActionError::fatal)?;
    }
    Ok(rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(ActionError::fatal)?
        .with_root_certificates(roots)
        .with_no_client_auth())
}

fn login_parameters(credentials: &JujuCredentials) -> Value {
    match credentials {
        JujuCredentials::Token(token) => {
            json!({"token": STANDARD.encode(token), "client-version": CLIENT_VERSION})
        }
        JujuCredentials::Password { user, password } => json!({
            "auth-tag": format!("user-{user}"),
            "credentials": password,
            "client-version": CLIENT_VERSION,
        }),
    }
}

impl Connection {
    pub async fn open(
        endpoint: &JujuEndpoint,
        scope: Scope<'_>,
        timeout: Duration,
    ) -> Result<Self, ActionError> {
        let path = match scope {
            Scope::Controller => String::from("/api"),
            Scope::Model(uuid) => format!("/model/{uuid}/api"),
        };
        let connect = async {
            let tcp = TcpStream::connect((endpoint.address, JUJU_API_PORT))
                .await
                .map_err(ActionError::retryable)?;
            let server = ServerName::try_from(SERVER_NAME).map_err(ActionError::fatal)?;
            let stream = TlsConnector::from(Arc::new(tls(&endpoint.certificate_authority)?))
                .connect(server, tcp)
                .await
                .map_err(ActionError::retryable)?;
            let url = format!("wss://{}:{JUJU_API_PORT}{path}", endpoint.address);
            let (socket, _) =
                tokio_tungstenite::client_async(url, stream).await.map_err(ActionError::retryable)?;
            Ok::<_, ActionError>(socket)
        };
        let socket = tokio::time::timeout(timeout, connect)
            .await
            .map_err(|_| ActionError::retryable("the Juju controller did not answer in time"))??;
        let mut connection = Self { socket, next_request: 1, facades: Facades::default(), timeout };
        let login = connection
            .raw_call("Admin", ADMIN_VERSION, "Login", login_parameters(&endpoint.credentials))
            .await?;
        connection.facades = Facades::from_login(&login);
        Ok(connection)
    }

    #[must_use]
    pub fn login(&self) -> &LoginResult {
        self.facades.login()
    }

    pub async fn call(
        &mut self,
        facade: &str,
        request: &str,
        arguments: Value,
    ) -> Result<Value, ActionError> {
        let version = self.facades.version(facade)?;
        self.raw_call(facade, version, request, arguments).await
    }

    async fn raw_call(
        &mut self,
        facade: &str,
        version: u32,
        request: &str,
        arguments: Value,
    ) -> Result<Value, ActionError> {
        let id = self.next_request;
        self.next_request += 1;
        let message = json!({"request-id": id, "type": facade, "version": version, "request": request, "params": arguments});
        let exchange = async {
            self.socket.send(Message::text(message.to_string())).await.map_err(ActionError::retryable)?;
            while let Some(frame) = self.socket.next().await {
                let frame = frame.map_err(ActionError::retryable)?;
                let Message::Text(text) = frame else {
                    continue;
                };
                let reply: Value = serde_json::from_str(text.as_str()).map_err(ActionError::retryable)?;
                if reply["request-id"] == json!(id) {
                    return crate::facades::reply_result(facade, request, &reply);
                }
            }
            Err(ActionError::retryable("the Juju controller closed the connection"))
        };
        tokio::time::timeout(self.timeout, exchange)
            .await
            .map_err(|_| ActionError::retryable(format!("{facade}.{request} did not answer in time")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_login_carries_either_a_token_or_a_users_password() {
        let token = login_parameters(&JujuCredentials::Token(String::from("signed")));
        assert_eq!(token, json!({"token": "c2lnbmVk", "client-version": CLIENT_VERSION}));
        let password = login_parameters(&JujuCredentials::Password {
            user: String::from("admin"),
            password: String::from("secret"),
        });
        assert_eq!(
            password,
            json!({"auth-tag": "user-admin", "credentials": "secret", "client-version": CLIENT_VERSION})
        );
    }
}
