use amafu_domain::ObjectReference;
use amafu_domain::kinds::lxd_identity::{LxdIdentity, lxd_identity_name};
use amafu_interfaces::Consistency;
use amafu_interfaces::objects::get;
use axum::Json;
use axum::response::{IntoResponse, Response};
use http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::routes::ApiState;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LxdCertificate {
    pub certificate_pem: String,
}

fn unavailable(message: &str) -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"message": message}))).into_response()
}

pub async fn certificate(state: ApiState) -> Response {
    let reference = ObjectReference::of::<LxdIdentity>(None, lxd_identity_name());
    match get::<LxdIdentity>(state.store.as_ref(), &reference, Consistency::Linearizable).await {
        Ok(Some(identity)) => {
            Json(LxdCertificate { certificate_pem: identity.specification.certificate_pem }).into_response()
        }
        Ok(None) => unavailable("Amafu has not created its LXD identity yet"),
        Err(error) => unavailable(&error.to_string()),
    }
}
