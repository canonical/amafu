use amafu_domain::access::Verb;
use amafu_domain::kinds::juju_controller::JujuController;
use amafu_domain::kinds::kubernetes_cluster::KubernetesCluster;
use amafu_domain::kinds::signing_key::{SigningKey, SigningKeySpecification};
use amafu_domain::platform_access::{ISSUER, juju_access, juju_user, kubernetes_group};
use amafu_domain::registry::KindDescriptor;
use amafu_domain::{Object, ObjectReference, Resource};
use amafu_interfaces::objects::{get, list};
use amafu_interfaces::{Consistency, Store};
use amafu_token_issuer::{Claims, JwkSet, KeyPair, discovery_document};
use axum::Json;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use http::{HeaderMap, StatusCode, Uri};
use time::{Duration, OffsetDateTime};

use crate::handlers::{ApiError, ApiSuccess, Operation};
use crate::request_chain::RequestContext;
use crate::routes::ApiState;
use crate::wire::ErrorCode;

const TOKEN_LIFETIME: Duration = Duration::minutes(5);
const JWKS_PATH: &str = "/v1/jwks";

pub async fn json_web_keys(state: ApiState) -> Response {
    match published_keys(state.store.as_ref(), state.clock.now()).await {
        Ok(keys) => {
            let public_keys: Vec<_> = keys.iter().map(|(_, key)| key.public_key()).collect();
            Json(JwkSet::of(&public_keys)).into_response()
        }
        Err(message) => (StatusCode::SERVICE_UNAVAILABLE, message).into_response(),
    }
}

pub async fn discovery(headers: HeaderMap) -> Response {
    let host =
        headers.get(http::header::HOST).and_then(|host| host.to_str().ok()).unwrap_or("amafu.internal");
    Json(discovery_document(ISSUER, &format!("https://{host}{JWKS_PATH}"))).into_response()
}

pub async fn kubernetes_token(
    state: ApiState,
    context: RequestContext,
    target: Uri,
    name: String,
) -> Result<ApiSuccess, ApiError> {
    let (operation, cluster) = readable::<KubernetesCluster>(&state, context, &target, name).await?;
    let audience =
        cluster.status.uid.clone().ok_or_else(|| not_ready(&operation, "the cluster has no UID yet"))?;
    let principal = &operation.context.principal;
    let group = kubernetes_group(&principal.project, principal.role);
    let claims = Claims::new(ISSUER, &principal.name, &audience, state.clock.now(), TOKEN_LIFETIME)
        .with("groups", serde_json::json!([group]));
    sign(&state, &operation, &claims).await
}

pub async fn juju_token(
    state: ApiState,
    context: RequestContext,
    target: Uri,
    name: String,
) -> Result<ApiSuccess, ApiError> {
    let (operation, controller) = readable::<JujuController>(&state, context, &target, name).await?;
    let uuid = controller
        .status
        .uuid
        .clone()
        .ok_or_else(|| not_ready(&operation, "the controller has no UUID yet"))?;
    let models: Vec<String> = controller.status.models.iter().map(|model| model.uuid.clone()).collect();
    let principal = &operation.context.principal;
    let claims = Claims::new(ISSUER, &juju_user(&principal.name), &uuid, state.clock.now(), TOKEN_LIFETIME)
        .with("access", juju_access(principal.role, &uuid, &models));
    sign(&state, &operation, &claims).await
}

async fn readable<R: Resource>(
    state: &ApiState,
    context: RequestContext,
    target: &Uri,
    name: String,
) -> Result<(Operation, Object<R>), ApiError> {
    let operation = Operation::new(KindDescriptor::of::<R>(), Verb::Get, context, target, Some(name))?;
    operation.authorize()?;
    let reference: ObjectReference = operation.reference()?;
    let found = get::<R>(state.store.as_ref(), &reference, Consistency::Linearizable)
        .await
        .map_err(|error| operation.store_error(error))?
        .ok_or_else(|| {
            operation.error(
                StatusCode::NOT_FOUND,
                ErrorCode::NotFound,
                format!("no {} named {}", R::KIND, reference.name),
            )
        })?;
    Ok((operation, found))
}

async fn sign(state: &ApiState, operation: &Operation, claims: &Claims) -> Result<ApiSuccess, ApiError> {
    let now = state.clock.now();
    let key = published_keys(state.store.as_ref(), now)
        .await
        .map_err(|message| operation.error(StatusCode::SERVICE_UNAVAILABLE, ErrorCode::Unavailable, message))?
        .into_iter()
        .find(|(specification, _)| specification.signs_at(now))
        .map(|(_, key)| key)
        .ok_or_else(|| {
            operation.error(
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorCode::Unavailable,
                String::from("no signing key is active yet"),
            )
        })?;
    let issued = claims.sign(&key).map_err(|error| {
        operation.error(StatusCode::INTERNAL_SERVER_ERROR, ErrorCode::Internal, error.to_string())
    })?;
    operation.json(StatusCode::CREATED, &issued)
}

fn not_ready(operation: &Operation, message: &str) -> ApiError {
    operation.error(StatusCode::CONFLICT, ErrorCode::Conflict, message.to_owned())
}

async fn published_keys(
    store: &dyn Store,
    now: OffsetDateTime,
) -> Result<Vec<(SigningKeySpecification, KeyPair)>, String> {
    let keys = list::<SigningKey>(store, None, Consistency::Linearizable)
        .await
        .map_err(|error| error.to_string())?;
    Ok(keys
        .into_iter()
        .map(|key| key.specification)
        .filter(|specification| specification.is_published_at(now))
        .filter_map(|specification| {
            let pkcs8 = STANDARD.decode(&specification.private_key_pkcs8).ok()?;
            KeyPair::from_pkcs8(&pkcs8).ok().map(|key| (specification, key))
        })
        .collect())
}
