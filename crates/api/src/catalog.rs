use axum::Json;
use axum::response::{IntoResponse, Response};
use http::StatusCode;

use crate::routes::ApiState;

pub async fn image_catalog(state: ApiState) -> Response {
    match state.catalog.entries().await {
        Ok(entries) => Json(entries).into_response(),
        Err(error) => {
            (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"message": error.to_string()})))
                .into_response()
        }
    }
}
