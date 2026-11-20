use amafu_domain::access::Verb;
use amafu_domain::kinds::juju_controller::JujuController;
use amafu_domain::kinds::kubernetes_cluster::KubernetesCluster;
use amafu_domain::platform_access::{ISSUER, juju_access, juju_user, kubernetes_group, platform_user};
use amafu_domain::registry::KindDescriptor;
use amafu_domain::{Object, ObjectReference, Resource};
use std::sync::Arc;

use amafu_interfaces::Consistency;
use amafu_interfaces::objects::get;
use amafu_token_issuer::{Claims, Keyring, KeyringError, discovery_document};
use axum::Json;
use axum::response::{IntoResponse, Response};
use http::{HeaderMap, StatusCode, Uri};
use time::Duration;

use crate::handlers::{ApiError, ApiSuccess, Operation};
use crate::request_chain::RequestContext;
use crate::routes::ApiState;
use crate::wire::ErrorCode;

const TOKEN_LIFETIME: Duration = Duration::minutes(5);
const JWKS_PATH: &str = "/v1/jwks";

pub async fn json_web_keys(state: ApiState) -> Response {
    match Keyring::new(Arc::clone(&state.store)).published(state.clock.now()).await {
        Ok(keys) => Json(keys).into_response(),
        Err(error) => (StatusCode::SERVICE_UNAVAILABLE, error.to_string()).into_response(),
    }
}

pub async fn discovery(target: Uri, headers: HeaderMap) -> Response {
    Json(discovery_document(ISSUER, &format!("https://{}{JWKS_PATH}", served_authority(&target, &headers))))
        .into_response()
}

fn served_authority(target: &Uri, headers: &HeaderMap) -> String {
    target
        .authority()
        .map(ToString::to_string)
        .or_else(|| headers.get(http::header::HOST).and_then(|host| host.to_str().ok()).map(str::to_owned))
        .unwrap_or_else(|| String::from("amafu.internal"))
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
    let claims =
        Claims::new(ISSUER, &platform_user(&principal.name), &audience, state.clock.now(), TOKEN_LIFETIME)
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
    let issued =
        Keyring::new(Arc::clone(&state.store)).sign(claims, state.clock.now()).await.map_err(|error| {
            match error {
                KeyringError::Issuer(_) => {
                    operation.error(StatusCode::INTERNAL_SERVER_ERROR, ErrorCode::Internal, error.to_string())
                }
                KeyringError::Unreadable(_) | KeyringError::NoActiveKey => operation.error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    ErrorCode::Unavailable,
                    error.to_string(),
                ),
            }
        })?;
    operation.json(StatusCode::CREATED, &issued)
}

fn not_ready(operation: &Operation, message: &str) -> ApiError {
    operation.error(StatusCode::CONFLICT, ErrorCode::Conflict, message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keys_are_found_where_the_document_was_served() {
        let over_http2: Uri = "https://10.20.0.12:7443/.well-known/openid-configuration".parse().unwrap();
        assert_eq!(served_authority(&over_http2, &HeaderMap::new()), "10.20.0.12:7443");
        let mut headers = HeaderMap::new();
        headers.insert(http::header::HOST, "10.20.0.11:7443".parse().unwrap());
        let over_http1: Uri = "/.well-known/openid-configuration".parse().unwrap();
        assert_eq!(served_authority(&over_http1, &headers), "10.20.0.11:7443");
    }
}
