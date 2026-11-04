use std::collections::BTreeMap;
use std::sync::Arc;

use amafu_domain::{Object, ProjectName, Resource, Scope};
use axum::body::Body;
use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use futures::stream::BoxStream;
use http::header::{AUTHORIZATION, CONTENT_TYPE};
use http::{Method, Request, Response, StatusCode};
use serde::de::DeserializeOwned;
use tower::ServiceExt;
use uuid::Uuid;

use crate::node_identity::PeerCertificate;
use crate::server::{crypto_provider, parse_certificates};
use crate::wire::{
    CreateRequest, ErrorBody, ListResponse, NewObjectMetadata, REQUEST_ID_HEADER, UpdateRequest, WatchFrame,
};

pub type ResponseBody = BoxStream<'static, Result<Bytes, ClientError>>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClientError {
    #[error("the request could not be sent: {0}")]
    Transport(String),
    #[error("the API refused the request with status {status}: {}", body.message)]
    Api { status: u16, body: ErrorBody },
    #[error("the API answered status {status}: {body}")]
    UnexpectedStatus { status: u16, body: String },
    #[error("the response cannot be decoded: {0}")]
    Decode(String),
}

impl ClientError {
    #[must_use]
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Api { status, .. } | Self::UnexpectedStatus { status, .. } => Some(*status),
            Self::Transport(_) | Self::Decode(_) => None,
        }
    }

    #[must_use]
    pub fn is_not_found(&self) -> bool {
        self.status() == Some(StatusCode::NOT_FOUND.as_u16())
    }
}

#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    async fn send(&self, request: Request<Bytes>) -> Result<Response<ResponseBody>, ClientError>;
}

#[derive(Clone)]
pub struct RouterTransport {
    router: axum::Router,
    peer_certificate: PeerCertificate,
}

impl RouterTransport {
    #[must_use]
    pub fn new(router: axum::Router) -> Self {
        Self { router, peer_certificate: PeerCertificate::default() }
    }

    #[must_use]
    pub fn presenting(router: axum::Router, certificate: Vec<u8>) -> Self {
        Self { router, peer_certificate: PeerCertificate(Some(certificate)) }
    }
}

#[async_trait::async_trait]
impl Transport for RouterTransport {
    async fn send(&self, request: Request<Bytes>) -> Result<Response<ResponseBody>, ClientError> {
        let mut request = request.map(Body::from);
        request.extensions_mut().insert(self.peer_certificate.clone());
        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .map_err(|error| ClientError::Transport(error.to_string()))?;
        Ok(response.map(|body| {
            body.into_data_stream()
                .map(|chunk| chunk.map_err(|error| ClientError::Transport(error.to_string())))
                .boxed()
        }))
    }
}

#[derive(Clone)]
pub struct HttpTransport {
    client: reqwest::Client,
    base_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientIdentity {
    pub certificate_pem: String,
    pub private_key_pem: String,
}

impl HttpTransport {
    pub fn new(base_url: &str, certificate_authority_pem: Option<&[u8]>) -> Result<Self, ClientError> {
        Self::with_identity(base_url, certificate_authority_pem, None)
    }

    pub fn with_identity(
        base_url: &str,
        certificate_authority_pem: Option<&[u8]>,
        identity: Option<&ClientIdentity>,
    ) -> Result<Self, ClientError> {
        let invalid = |error: &dyn std::fmt::Display| ClientError::Transport(error.to_string());
        let mut roots = rustls::RootCertStore::empty();
        if let Some(pem) = certificate_authority_pem {
            for certificate in parse_certificates(pem).map_err(|error| invalid(&error))? {
                roots.add(certificate).map_err(|error| invalid(&error))?;
            }
        }
        let builder = rustls::ClientConfig::builder_with_provider(crypto_provider())
            .with_safe_default_protocol_versions()
            .map_err(|error| invalid(&error))?
            .with_root_certificates(roots);
        let mut tls = match identity {
            Some(identity) => {
                let chain = parse_certificates(identity.certificate_pem.as_bytes())
                    .map_err(|error| invalid(&error))?;
                let key = rustls_pemfile::private_key(&mut identity.private_key_pem.as_bytes())
                    .map_err(|error| invalid(&error))?
                    .ok_or_else(|| {
                        ClientError::Transport(String::from("the client identity holds no key"))
                    })?;
                builder.with_client_auth_cert(chain, key).map_err(|error| invalid(&error))?
            }
            None => builder.with_no_client_auth(),
        };
        tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let client =
            reqwest::Client::builder().use_preconfigured_tls(tls).build().map_err(|error| invalid(&error))?;
        Ok(Self { client, base_url: base_url.trim_end_matches('/').to_owned() })
    }
}

#[async_trait::async_trait]
impl Transport for HttpTransport {
    async fn send(&self, request: Request<Bytes>) -> Result<Response<ResponseBody>, ClientError> {
        let (parts, body) = request.into_parts();
        let path = parts.uri.path_and_query().map_or("/", http::uri::PathAndQuery::as_str);
        let response = self
            .client
            .request(parts.method, format!("{}{path}", self.base_url))
            .headers(parts.headers)
            .body(body)
            .send()
            .await
            .map_err(|error| ClientError::Transport(describe(&error)))?;
        let mut builder = Response::builder().status(response.status());
        for (name, value) in response.headers() {
            builder = builder.header(name, value);
        }
        let body: ResponseBody = response
            .bytes_stream()
            .map(|chunk| chunk.map_err(|error| ClientError::Transport(error.to_string())))
            .boxed();
        builder.body(body).map_err(|error| ClientError::Transport(error.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawResponse {
    pub status: u16,
    pub request_id: Option<Uuid>,
    pub body: Bytes,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WatchUpdate<R: Resource> {
    Changed(Object<R>),
    Removed,
}

#[derive(Clone)]
pub struct ApiClient {
    transport: Arc<dyn Transport>,
    token: Option<String>,
    project: Option<ProjectName>,
}

fn describe(error: &dyn std::error::Error) -> String {
    let mut description = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        description.push_str(": ");
        description.push_str(&cause.to_string());
        source = cause.source();
    }
    description
}

pub(crate) async fn collect(body: ResponseBody) -> Result<Bytes, ClientError> {
    let chunks: Vec<Bytes> = body.collect::<Vec<_>>().await.into_iter().collect::<Result<_, _>>()?;
    Ok(chunks.concat().into())
}

pub(crate) fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T, ClientError> {
    serde_json::from_slice(body).map_err(|error| ClientError::Decode(error.to_string()))
}

pub(crate) fn refusal(status: u16, body: &[u8]) -> ClientError {
    match serde_json::from_slice::<ErrorBody>(body) {
        Ok(body) => ClientError::Api { status, body },
        Err(_) => ClientError::UnexpectedStatus { status, body: String::from_utf8_lossy(body).into_owned() },
    }
}

impl ApiClient {
    #[must_use]
    pub fn new(transport: Arc<dyn Transport>, token: Option<String>) -> Self {
        Self { transport, token, project: None }
    }

    #[must_use]
    pub fn in_project(mut self, project: ProjectName) -> Self {
        self.project = Some(project);
        self
    }

    #[must_use]
    pub fn project(&self) -> Option<&ProjectName> {
        self.project.as_ref()
    }

    fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<Request<Bytes>, ClientError> {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(token) = &self.token {
            builder = builder.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        if body.is_some() {
            builder = builder.header(CONTENT_TYPE, "application/json");
        }
        builder
            .body(body.map(Bytes::from).unwrap_or_default())
            .map_err(|error| ClientError::Transport(error.to_string()))
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<Response<ResponseBody>, ClientError> {
        self.transport.send(self.request(method, path, body)?).await
    }

    pub async fn raw(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<RawResponse, ClientError> {
        let response = self.send(method, path, body).await?;
        let status = response.status().as_u16();
        let request_id = response
            .headers()
            .get(REQUEST_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| Uuid::parse_str(value).ok());
        let body = collect(response.into_body()).await?;
        Ok(RawResponse { status, request_id, body })
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<T, ClientError> {
        let response = self.raw(method, path, body).await?;
        if !(200..300).contains(&response.status) {
            return Err(refusal(response.status, &response.body));
        }
        decode(&response.body)
    }

    fn project_query<R: Resource>(&self) -> String {
        match (R::SCOPE, &self.project) {
            (Scope::Project, Some(project)) => format!("?project={project}"),
            _ => String::new(),
        }
    }

    fn collection_path<R: Resource>(&self) -> String {
        format!("/v1/{}{}", R::PLURAL_NAME, self.project_query::<R>())
    }

    fn object_path<R: Resource>(&self, name: &str, suffix: &str) -> String {
        format!("/v1/{}/{name}{suffix}{}", R::PLURAL_NAME, self.project_query::<R>())
    }

    fn encode(value: &impl serde::Serialize) -> Result<Vec<u8>, ClientError> {
        serde_json::to_vec(value).map_err(|error| ClientError::Decode(error.to_string()))
    }

    pub async fn create<R: Resource>(
        &self,
        name: &amafu_domain::ObjectName,
        specification: &R::Specification,
    ) -> Result<Object<R>, ClientError> {
        let body = CreateRequest {
            metadata: NewObjectMetadata { name: name.clone(), labels: BTreeMap::new() },
            specification: serde_json::to_value(specification)
                .map_err(|error| ClientError::Decode(error.to_string()))?,
        };
        self.call(Method::POST, &self.collection_path::<R>(), Some(Self::encode(&body)?)).await
    }

    pub async fn get<R: Resource>(&self, name: &str) -> Result<Object<R>, ClientError> {
        self.call(Method::GET, &self.object_path::<R>(name, ""), None).await
    }

    pub async fn list_in_every_project<R: Resource>(&self) -> Result<Vec<Object<R>>, ClientError> {
        let path = format!("/v1/{}?all-projects=true", R::PLURAL_NAME);
        let response: ListResponse = self.call(Method::GET, &path, None).await?;
        response
            .items
            .into_iter()
            .map(|item| serde_json::from_value(item).map_err(|error| ClientError::Decode(error.to_string())))
            .collect()
    }

    pub async fn list<R: Resource>(&self) -> Result<Vec<Object<R>>, ClientError> {
        let response: ListResponse = self.call(Method::GET, &self.collection_path::<R>(), None).await?;
        response
            .items
            .into_iter()
            .map(|item| serde_json::from_value(item).map_err(|error| ClientError::Decode(error.to_string())))
            .collect()
    }

    pub async fn update<R: Resource>(
        &self,
        name: &str,
        specification: &R::Specification,
        expected_generation: Option<u64>,
    ) -> Result<Object<R>, ClientError> {
        let body = UpdateRequest {
            expected_generation,
            specification: serde_json::to_value(specification)
                .map_err(|error| ClientError::Decode(error.to_string()))?,
        };
        self.call(Method::PUT, &self.object_path::<R>(name, ""), Some(Self::encode(&body)?)).await
    }

    pub async fn delete<R: Resource>(&self, name: &str) -> Result<Object<R>, ClientError> {
        self.call(Method::DELETE, &self.object_path::<R>(name, ""), None).await
    }

    pub async fn watch<R: Resource>(
        &self,
        name: &str,
    ) -> Result<BoxStream<'static, Result<WatchUpdate<R>, ClientError>>, ClientError> {
        let response = self.send(Method::GET, &self.object_path::<R>(name, "/watch"), None).await?;
        let status = response.status().as_u16();
        if status != StatusCode::OK.as_u16() {
            let body = collect(response.into_body()).await?;
            return Err(refusal(status, &body));
        }
        Ok(watch_updates::<R>(response.into_body()))
    }
}

fn watch_updates<R: Resource>(body: ResponseBody) -> BoxStream<'static, Result<WatchUpdate<R>, ClientError>> {
    futures::stream::unfold((body, BytesMut::new()), |(mut body, mut buffer)| async move {
        loop {
            if let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
                let line = buffer.split_to(position + 1);
                let update = decode::<WatchFrame>(&line[..position]).and_then(|frame| match frame {
                    WatchFrame::Object { object } => serde_json::from_value(object)
                        .map(WatchUpdate::Changed)
                        .map_err(|error| ClientError::Decode(error.to_string())),
                    WatchFrame::Removed => Ok(WatchUpdate::Removed),
                });
                return Some((update, (body, buffer)));
            }
            match body.next().await? {
                Ok(chunk) => buffer.extend_from_slice(&chunk),
                Err(error) => return Some((Err(error), (body, buffer))),
            }
        }
    })
    .boxed()
}
