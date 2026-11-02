use std::sync::Arc;

use amafu_domain::registry;
use amafu_interfaces::{AuditLog, Clock, Identifiers, Store};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Extension, Path, State};
use axum::routing::get;
use axum::{Router, middleware};
use http::Uri;

use crate::authentication::Authenticator;
use crate::handlers::{create_object, delete_object, get_object, list_objects, update_object, watch_object};
use crate::request_chain::{RequestContext, request_chain};
use crate::wire::MAXIMUM_BODY_BYTES;

#[derive(Clone)]
pub struct ApiState {
    pub store: Arc<dyn Store>,
    pub authenticator: Arc<dyn Authenticator>,
    pub audit: Arc<dyn AuditLog>,
    pub clock: Arc<dyn Clock>,
    pub identifiers: Arc<dyn Identifiers>,
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
    router
        .layer(DefaultBodyLimit::max(MAXIMUM_BODY_BYTES))
        .layer(middleware::from_fn_with_state(state.clone(), request_chain))
        .with_state(state)
}
