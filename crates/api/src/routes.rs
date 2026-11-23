use std::sync::Arc;

use amafu_domain::platform_access::JWKS_PATH;
use amafu_domain::registry;
use amafu_interfaces::{AuditLog, Clock, Identifiers, ImageCatalog, Store};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Extension, Path, State};
use axum::routing::{get, post};
use axum::{Router, middleware};
use http::Uri;

use crate::authentication::Authenticator;
use crate::handlers::{create_object, delete_object, get_object, list_objects, update_object, watch_object};
use crate::key_directory::{self, KeyDirectory};
use crate::platform_tokens;
use crate::request_chain::{RequestContext, request_chain};
use crate::service_accounts::{self, IssuerDirectory};
use crate::wire::MAXIMUM_BODY_BYTES;

#[derive(Clone)]
pub struct ApiState {
    pub store: Arc<dyn Store>,
    pub authenticator: Arc<dyn Authenticator>,
    pub audit: Arc<dyn AuditLog>,
    pub clock: Arc<dyn Clock>,
    pub identifiers: Arc<dyn Identifiers>,
    pub issuers: Arc<dyn IssuerDirectory>,
    pub catalog: Arc<dyn ImageCatalog>,
    pub key_directory: Arc<dyn KeyDirectory>,
}

type Context = Extension<RequestContext>;

pub fn router(state: ApiState) -> Router {
    let mut router = Router::new();
    for kind in registry::kinds() {
        let collection = format!("/v1/{}", kind.plural_name);
        router = router
            .route(
                &collection,
                get(move |State(state): State<ApiState>, Extension(context): Context, target: Uri| {
                    list_objects(kind, state, context, target)
                })
                .post(
                    move |State(state): State<ApiState>,
                          Extension(context): Context,
                          target: Uri,
                          body: Bytes| {
                        create_object(kind, state, context, target, body)
                    },
                ),
            )
            .route(
                &format!("{collection}/{{name}}"),
                get(
                    move |State(state): State<ApiState>,
                          Extension(context): Context,
                          target: Uri,
                          Path(name): Path<String>| {
                        get_object(kind, state, context, target, name)
                    },
                )
                .put(
                    move |State(state): State<ApiState>,
                          Extension(context): Context,
                          target: Uri,
                          Path(name): Path<String>,
                          body: Bytes| {
                        update_object(kind, state, context, target, name, body)
                    },
                )
                .delete(
                    move |State(state): State<ApiState>,
                          Extension(context): Context,
                          target: Uri,
                          Path(name): Path<String>| {
                        delete_object(kind, state, context, target, name)
                    },
                ),
            )
            .route(
                &format!("{collection}/{{name}}/watch"),
                get(
                    move |State(state): State<ApiState>,
                          Extension(context): Context,
                          target: Uri,
                          Path(name): Path<String>| {
                        watch_object(kind, state, context, target, name)
                    },
                ),
            );
    }
    credential_routes(router)
        .layer(DefaultBodyLimit::max(MAXIMUM_BODY_BYTES))
        .layer(middleware::from_fn_with_state(state.clone(), request_chain))
        .merge(public_router(state.clone()))
        .with_state(state)
}

fn credential_routes(router: Router<ApiState>) -> Router<ApiState> {
    router
        .route("/v1/image-catalog", get(|State(state): State<ApiState>| crate::catalog::image_catalog(state)))
        .route("/v1/lxd-certificate", get(|State(state): State<ApiState>| crate::lxd::certificate(state)))
        .route(
            "/v1/ssh-key-sources/{source}/{account}",
            get(
                |State(state): State<ApiState>,
                 Extension(context): Context,
                 target: Uri,
                 Path((source, account)): Path<(String, String)>| {
                    key_directory::look_up(state, context, target, source, account)
                },
            ),
        )
        .route(
            "/v1/kubernetes-clusters/{name}/tokens",
            post(
                |State(state): State<ApiState>,
                 Extension(context): Context,
                 target: Uri,
                 Path(name): Path<String>| {
                    platform_tokens::kubernetes_token(state, context, target, name)
                },
            ),
        )
        .route(
            "/v1/juju-controllers/{name}/tokens",
            post(
                |State(state): State<ApiState>,
                 Extension(context): Context,
                 target: Uri,
                 Path(name): Path<String>| {
                    platform_tokens::juju_token(state, context, target, name)
                },
            ),
        )
        .route(
            "/v1/service-accounts/{name}/secrets",
            post(
                |State(state): State<ApiState>,
                 Extension(context): Context,
                 target: Uri,
                 Path(name): Path<String>| {
                    service_accounts::create_secret(state, context, target, name)
                },
            ),
        )
        .route(
            "/v1/service-accounts/{name}/secrets/{id}",
            axum::routing::delete(
                |State(state): State<ApiState>,
                 Extension(context): Context,
                 target: Uri,
                 Path((name, id)): Path<(String, String)>| {
                    service_accounts::delete_secret(state, context, target, name, id)
                },
            ),
        )
}

fn public_router(state: ApiState) -> Router<ApiState> {
    Router::new()
        .route(JWKS_PATH, get(move || platform_tokens::json_web_keys(state.clone())))
        .route("/.well-known/openid-configuration", get(platform_tokens::discovery))
        .route(
            "/v1/tokens",
            post(|State(state): State<ApiState>, body| service_accounts::exchange(state, body)),
        )
}
