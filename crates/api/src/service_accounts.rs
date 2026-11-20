use std::sync::Arc;

use amafu_domain::access::{Role, Verb};
use amafu_domain::kinds::service_account::{
    ClientSecret, MAXIMUM_SECRETS, ServiceAccount, ServiceAccountSpecification,
};
use amafu_domain::platform_access::ISSUER;
use amafu_domain::registry::KindDescriptor;
use amafu_domain::{Object, ObjectName, ObjectReference, ProjectName};
use amafu_interfaces::objects::{get, update_status};
use amafu_interfaces::{Consistency, Store};
use amafu_token_issuer::{Claims, ExternalKeys, Keyring, audiences, unverified_claims, verify_external};
use axum::Json;
use axum::response::{IntoResponse, Response};
use http::{HeaderMap, StatusCode, Uri};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use time::{Duration, OffsetDateTime};

use crate::authentication::{AuthenticationError, Authenticator, bearer_token};
use crate::handlers::{ApiError, ApiSuccess, Operation};
use crate::identity::Principal;
use crate::request_chain::RequestContext;
use crate::routes::ApiState;
use crate::wire::ErrorCode;

pub const API_AUDIENCE: &str = "amafu-api";
const API_TOKEN_LIFETIME: Duration = Duration::minutes(15);
const ISSUER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const SECRET_PREFIX: &str = "amafu-secret-";
const CLIENT_CREDENTIALS: &str = "client_credentials";
const TOKEN_EXCHANGE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";

#[async_trait::async_trait]
pub trait IssuerDirectory: Send + Sync {
    async fn keys(&self, issuer: &str) -> Result<ExternalKeys, String>;
}

#[derive(Debug, Clone)]
pub struct HttpIssuerDirectory {
    client: Option<reqwest::Client>,
}

impl Default for HttpIssuerDirectory {
    fn default() -> Self {
        let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
        let client =
            rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .ok()
                .map(|builder| builder.with_root_certificates(roots).with_no_client_auth())
                .and_then(|tls| {
                    reqwest::Client::builder()
                        .use_preconfigured_tls(tls)
                        .https_only(true)
                        .timeout(ISSUER_TIMEOUT)
                        .build()
                        .ok()
                });
        Self { client }
    }
}

impl HttpIssuerDirectory {
    async fn document(&self, url: &str) -> Result<serde_json::Value, String> {
        let client = self.client.as_ref().ok_or("no HTTPS client could be built")?;
        let response = client.get(url).send().await.map_err(|error| error.to_string())?;
        if !response.status().is_success() {
            return Err(format!("{url} answered {}", response.status()));
        }
        response.json().await.map_err(|error| error.to_string())
    }
}

#[async_trait::async_trait]
impl IssuerDirectory for HttpIssuerDirectory {
    async fn keys(&self, issuer: &str) -> Result<ExternalKeys, String> {
        let discovery = self
            .document(&format!("{}/.well-known/openid-configuration", issuer.trim_end_matches('/')))
            .await?;
        let jwks = discovery["jwks_uri"].as_str().ok_or("the issuer publishes no jwks_uri")?;
        if !jwks.starts_with("https://") {
            return Err(String::from("the issuer's keys are not served over https"));
        }
        Ok(ExternalKeys::parse(&self.document(jwks).await?))
    }
}

pub struct IssuedTokenAuthenticator {
    keyring: Keyring,
}

impl IssuedTokenAuthenticator {
    #[must_use]
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { keyring: Keyring::new(store) }
    }
}

fn principal_of(claims: &Claims) -> Option<Principal> {
    Some(Principal {
        name: claims.sub.clone(),
        role: serde_json::from_value(claims.extra.get("role")?.clone()).ok()?,
        project: ProjectName::new(claims.extra.get("project")?.as_str()?).ok()?,
    })
}

#[async_trait::async_trait]
impl Authenticator for IssuedTokenAuthenticator {
    async fn authenticate(
        &self,
        headers: &HeaderMap,
        now: OffsetDateTime,
    ) -> Result<Principal, AuthenticationError> {
        let token = bearer_token(headers)?;
        if token.split('.').count() != 3 {
            return Err(AuthenticationError::MalformedCredentials);
        }
        let keys =
            self.keyring.public_keys(now).await.map_err(|_| AuthenticationError::UnknownCredentials)?;
        let claims = keys
            .iter()
            .find_map(|key| Claims::verified(token, key).ok())
            .filter(|claims| claims.iss == ISSUER && claims.aud == API_AUDIENCE)
            .ok_or(AuthenticationError::UnknownCredentials)?;
        if claims.exp <= now.unix_timestamp() {
            return Err(AuthenticationError::ExpiredCredentials);
        }
        principal_of(&claims).ok_or(AuthenticationError::UnknownCredentials)
    }
}

#[derive(Debug, Serialize)]
pub struct CreatedSecret {
    pub id: String,
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Debug, Deserialize)]
pub struct TokenRequest {
    pub grant_type: String,
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub subject_token: Option<String>,
}

fn digest(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

fn client_id(project: &ProjectName, name: &ObjectName) -> String {
    format!("{project}/{name}")
}

fn service_subject(project: &ProjectName, name: &ObjectName) -> String {
    format!("service-account:{project}/{name}")
}

fn writable(
    context: RequestContext,
    target: &Uri,
    name: String,
) -> Result<(Operation, ObjectReference), ApiError> {
    let operation =
        Operation::new(KindDescriptor::of::<ServiceAccount>(), Verb::Update, context, target, Some(name))?;
    operation.authorize()?;
    let reference = operation.reference()?;
    Ok((operation, reference))
}

pub async fn create_secret(
    state: ApiState,
    context: RequestContext,
    target: Uri,
    name: String,
) -> Result<ApiSuccess, ApiError> {
    let (operation, reference) = writable(context, &target, name)?;
    let id = state.identifiers.generate().simple().to_string()[..12].to_owned();
    let secret = format!(
        "{SECRET_PREFIX}{}{}",
        state.identifiers.generate().simple(),
        state.identifiers.generate().simple()
    );
    let recorded = ClientSecret { id: id.clone(), hash: digest(&secret), created_at: state.clock.now() };
    let updated = update_status::<ServiceAccount>(state.store.as_ref(), &reference, |account| {
        (account.status.secrets.len() < MAXIMUM_SECRETS).then(|| {
            let mut status = account.status.clone();
            status.secrets.push(recorded.clone());
            status
        })
    })
    .await
    .map_err(|error| operation.store_error(error))?;
    if !updated.status.secrets.iter().any(|known| known.id == id) {
        return Err(operation.error(
            StatusCode::CONFLICT,
            ErrorCode::Conflict,
            format!("a service account holds at most {MAXIMUM_SECRETS} secrets; delete one first"),
        ));
    }
    let project =
        updated.metadata.project.clone().unwrap_or_else(|| operation.context.principal.project.clone());
    let created =
        CreatedSecret { id, client_id: client_id(&project, &updated.metadata.name), client_secret: secret };
    operation.json(StatusCode::CREATED, &created)
}

pub async fn delete_secret(
    state: ApiState,
    context: RequestContext,
    target: Uri,
    name: String,
    id: String,
) -> Result<ApiSuccess, ApiError> {
    let (operation, reference) = writable(context, &target, name)?;
    let updated = update_status::<ServiceAccount>(state.store.as_ref(), &reference, |account| {
        account.status.secrets.iter().any(|secret| secret.id == id).then(|| {
            let mut status = account.status.clone();
            status.secrets.retain(|secret| secret.id != id);
            status
        })
    })
    .await
    .map_err(|error| operation.store_error(error))?;
    operation.json(StatusCode::OK, &json!({"id": id, "remaining": updated.status.secrets.len()}))
}

fn refused(error: &str, description: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({"error": error, "error_description": description}))).into_response()
}

async fn account(store: &dyn Store, client: &str) -> Option<(ProjectName, Object<ServiceAccount>)> {
    let (project, name) = client.split_once('/')?;
    let project = ProjectName::new(project).ok()?;
    let reference = ObjectReference::of::<ServiceAccount>(Some(project.clone()), ObjectName::new(name).ok()?);
    let found = get::<ServiceAccount>(store, &reference, Consistency::Linearizable).await.ok().flatten()?;
    Some((project, found))
}

fn knows_secret(account: &Object<ServiceAccount>, secret: &str) -> bool {
    let presented = digest(secret);
    account
        .status
        .secrets
        .iter()
        .fold(false, |found, known| found | bool::from(known.hash.as_bytes().ct_eq(presented.as_bytes())))
}

fn trusts(specification: &ServiceAccountSpecification, claims: &serde_json::Value) -> bool {
    let issuer = claims["iss"].as_str().unwrap_or_default();
    let subject = claims["sub"].as_str().unwrap_or_default();
    let presented = audiences(claims);
    specification.federated_identities.iter().any(|identity| {
        identity.issuer == issuer
            && identity.subject == subject
            && identity.audience.as_ref().is_none_or(|audience| presented.contains(audience))
    })
}

async fn federated(
    state: &ApiState,
    account: &Object<ServiceAccount>,
    token: &str,
) -> Result<(), &'static str> {
    let claims = unverified_claims(token).map_err(|_| "the subject token is malformed")?;
    if !trusts(&account.specification, &claims) {
        return Err("the service account does not trust this issuer and subject");
    }
    let issuer = claims["iss"].as_str().unwrap_or_default();
    let keys = state.issuers.keys(issuer).await.map_err(|_| "the issuer's keys could not be fetched")?;
    let verified =
        verify_external(token, &keys, state.clock.now()).map_err(|_| "the subject token does not verify")?;
    if trusts(&account.specification, &verified) { Ok(()) } else { Err("the subject token does not verify") }
}

fn mint(state: &ApiState, project: &ProjectName, account: &Object<ServiceAccount>, role: Role) -> Claims {
    let subject = service_subject(project, &account.metadata.name);
    Claims::new(ISSUER, &subject, API_AUDIENCE, state.clock.now(), API_TOKEN_LIFETIME)
        .with("role", json!(role))
        .with("project", json!(project.to_string()))
}

pub async fn exchange(state: ApiState, Json(request): Json<TokenRequest>) -> Response {
    let Some((project, account)) = account(state.store.as_ref(), &request.client_id).await else {
        return refused("invalid_client", "no such service account");
    };
    let admitted = match request.grant_type.as_str() {
        CLIENT_CREDENTIALS => match &request.client_secret {
            Some(secret) if knows_secret(&account, secret) => Ok(()),
            _ => Err("the client secret is not recognized"),
        },
        TOKEN_EXCHANGE => match &request.subject_token {
            Some(token) => federated(&state, &account, token).await,
            None => Err("a token exchange needs a subject_token"),
        },
        _ => return refused("unsupported_grant_type", "use client_credentials or token-exchange"),
    };
    if let Err(description) = admitted {
        tracing::warn!(client = %request.client_id, description, "a service account token was refused");
        return refused("invalid_client", description);
    }
    let claims = mint(&state, &project, &account, account.specification.role);
    match Keyring::new(Arc::clone(&state.store)).sign(&claims, state.clock.now()).await {
        Ok(issued) => {
            tracing::info!(client = %request.client_id, "a service account token was issued");
            Json(json!({
                "access_token": issued.token,
                "token_type": "Bearer",
                "expires_in": API_TOKEN_LIFETIME.whole_seconds(),
            }))
            .into_response()
        }
        Err(error) => (StatusCode::SERVICE_UNAVAILABLE, error.to_string()).into_response(),
    }
}
