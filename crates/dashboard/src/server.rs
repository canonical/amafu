use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use amafu_api::{ClientError, Transport};
use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE, COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use bytes::Bytes;
use futures::StreamExt;
use http_body_util::BodyExt;
use serde::Deserialize;
use time::OffsetDateTime;

use crate::session::{Session, SessionKey, session_lasting};

pub const SESSION_COOKIE: &str = "amafu-session";
pub const DASHBOARD_HEADER: &str = "x-amafu-dashboard";
const LARGEST_REQUEST_BYTES: usize = 1024 * 1024;
const LARGEST_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const INDEX: &str = include_str!("../assets/index.html");
const SCRIPT: &str = include_str!("../assets/app.js");
const STYLE: &str = include_str!("../assets/app.css");
const LOGO: &str = include_str!("../assets/logo.svg");
const FONTS: [(&str, &[u8]); 3] = [
    ("ubuntu.woff2", include_bytes!("../assets/fonts/ubuntu.woff2")),
    ("ubuntu-italic.woff2", include_bytes!("../assets/fonts/ubuntu-italic.woff2")),
    ("ubuntu-mono.woff2", include_bytes!("../assets/fonts/ubuntu-mono.woff2")),
];
const IMMUTABLE: &str = "public, max-age=31536000, immutable";

#[derive(Clone)]
pub struct DashboardState {
    pub key: Arc<SessionKey>,
    pub upstreams: Arc<Vec<Arc<dyn Transport>>>,
    pub preferred: Arc<AtomicUsize>,
    pub session_lifetime: Duration,
}

impl DashboardState {
    #[must_use]
    pub fn new(key: SessionKey, upstreams: Vec<Arc<dyn Transport>>, session_lifetime: Duration) -> Self {
        Self {
            key: Arc::new(key),
            upstreams: Arc::new(upstreams),
            preferred: Arc::new(AtomicUsize::new(0)),
            session_lifetime,
        }
    }

    async fn forward(&self, request: http::Request<Bytes>) -> Result<http::Response<Bytes>, ClientError> {
        collect(self.open(request).await?).await
    }

    pub(crate) async fn open(
        &self,
        request: http::Request<Bytes>,
    ) -> Result<http::Response<amafu_api::client::ResponseBody>, ClientError> {
        let count = self.upstreams.len();
        let first = self.preferred.load(Ordering::Relaxed);
        let mut last_failure = ClientError::Transport(String::from("the dashboard knows no core member"));
        for offset in 0..count {
            let index = (first + offset) % count;
            match self.upstreams[index].send(request.clone()).await {
                Ok(response) => {
                    self.preferred.store(index, Ordering::Relaxed);
                    return Ok(response);
                }
                Err(failure @ ClientError::Transport(_)) => last_failure = failure,
                Err(other) => return Err(other),
            }
        }
        Err(last_failure)
    }
}

pub(crate) async fn collect(
    response: http::Response<amafu_api::client::ResponseBody>,
) -> Result<http::Response<Bytes>, ClientError> {
    let (parts, mut body) = response.into_parts();
    let mut collected = Vec::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk?;
        if collected.len() + chunk.len() > LARGEST_RESPONSE_BYTES {
            return Err(ClientError::Transport(String::from("the cluster's answer is too large to forward")));
        }
        collected.extend_from_slice(&chunk);
    }
    Ok(http::Response::from_parts(parts, Bytes::from(collected)))
}

fn asset(content_type: &'static str, contents: &'static str) -> Response {
    ([(CONTENT_TYPE, content_type), (http::header::CACHE_CONTROL, "no-store")], contents).into_response()
}

async fn font(Path(name): Path<String>) -> Response {
    match FONTS.iter().find(|(file, _)| *file == name) {
        Some((_, contents)) => {
            ([(CONTENT_TYPE, "font/woff2"), (http::header::CACHE_CONTROL, IMMUTABLE)], *contents)
                .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn page(method: Method) -> Response {
    if method == Method::GET {
        asset("text/html; charset=utf-8", INDEX)
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

pub(crate) fn refusal(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({"message": message}))).into_response()
}

fn session_cookie(value: &str, max_age: Duration) -> String {
    format!(
        "{SESSION_COOKIE}={value}; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age={}",
        max_age.as_secs()
    )
}

fn cookie_value<'headers>(headers: &'headers HeaderMap, name: &str) -> Option<&'headers str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|pair| pair.trim().strip_prefix(name)?.strip_prefix('='))
}

pub(crate) fn current_session(state: &DashboardState, headers: &HeaderMap) -> Option<Session> {
    let cookie = cookie_value(headers, SESSION_COOKIE)?;
    state.key.open(cookie, OffsetDateTime::now_utc()).ok()
}

#[derive(Deserialize)]
struct SignIn {
    token: String,
    #[serde(default)]
    project: Option<String>,
}

pub(crate) fn upstream_request(
    method: Method,
    path_and_query: &str,
    token: &str,
    body: Bytes,
) -> Option<http::Request<Bytes>> {
    let mut request = http::Request::builder().method(method).uri(path_and_query);
    if let Ok(value) = HeaderValue::from_str(&format!("Bearer {token}")) {
        request = request.header(AUTHORIZATION, value);
    }
    if !body.is_empty() {
        request = request.header(CONTENT_TYPE, "application/json");
    }
    request.body(body).ok()
}

async fn sign_in(State(state): State<DashboardState>, Json(sign_in): Json<SignIn>) -> Response {
    let probe_path = match &sign_in.project {
        Some(project) => format!("/v1/vms?project={project}"),
        None => String::from("/v1/nodes"),
    };
    let Some(probe) = upstream_request(Method::GET, &probe_path, &sign_in.token, Bytes::new()) else {
        return refusal(StatusCode::BAD_REQUEST, "the sign-in cannot be checked");
    };
    match state.forward(probe).await {
        Ok(answer) if answer.status() == StatusCode::UNAUTHORIZED => {
            refusal(StatusCode::UNAUTHORIZED, "the cluster does not know this token")
        }
        Ok(_) => {
            let session = session_lasting(
                sign_in.token,
                sign_in.project,
                OffsetDateTime::now_utc(),
                state.session_lifetime,
            );
            match state.key.seal(&session) {
                Ok(sealed) => {
                    let mut response = StatusCode::NO_CONTENT.into_response();
                    if let Ok(cookie) =
                        HeaderValue::from_str(&session_cookie(&sealed, state.session_lifetime))
                    {
                        response.headers_mut().insert(SET_COOKIE, cookie);
                    }
                    response
                }
                Err(error) => refusal(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
            }
        }
        Err(error) => refusal(StatusCode::BAD_GATEWAY, &error.to_string()),
    }
}

async fn sign_out() -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    if let Ok(cookie) = HeaderValue::from_str(&session_cookie("", Duration::ZERO)) {
        response.headers_mut().insert(SET_COOKIE, cookie);
    }
    response
}

async fn who_am_i(State(state): State<DashboardState>, headers: HeaderMap) -> Response {
    match current_session(&state, &headers) {
        Some(session) => Json(serde_json::json!({"project": session.project})).into_response(),
        None => refusal(StatusCode::UNAUTHORIZED, "sign in first"),
    }
}

async fn proxy(State(state): State<DashboardState>, Path(path): Path<String>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let Some(session) = current_session(&state, &parts.headers) else {
        return refusal(StatusCode::UNAUTHORIZED, "sign in first");
    };
    if parts.method != Method::GET && !parts.headers.contains_key(DASHBOARD_HEADER) {
        return refusal(StatusCode::FORBIDDEN, "changes must come from the dashboard itself");
    }
    let collected =
        axum::body::to_bytes(Body::new(body.map_err(axum::Error::new)), LARGEST_REQUEST_BYTES).await;
    let Ok(body) = collected else {
        return refusal(StatusCode::PAYLOAD_TOO_LARGE, "the request is too large");
    };
    let query = parts.uri.query().map(|query| format!("?{query}")).unwrap_or_default();
    let Some(upstream) = upstream_request(parts.method, &format!("/{path}{query}"), &session.token, body)
    else {
        return refusal(StatusCode::BAD_REQUEST, "the request cannot be forwarded");
    };
    match state.forward(upstream).await {
        Ok(answer) => {
            let (parts, body) = answer.into_parts();
            let mut response = Response::new(Body::from(body));
            *response.status_mut() = parts.status;
            if let Some(content_type) = parts.headers.get(CONTENT_TYPE) {
                response.headers_mut().insert(CONTENT_TYPE, content_type.clone());
            }
            response
        }
        Err(error) => refusal(StatusCode::BAD_GATEWAY, &error.to_string()),
    }
}

pub fn dashboard_router(state: DashboardState) -> axum::Router {
    axum::Router::new()
        .route("/", get(|| async { asset("text/html; charset=utf-8", INDEX) }))
        .route("/app.js", get(|| async { asset("text/javascript; charset=utf-8", SCRIPT) }))
        .route("/app.css", get(|| async { asset("text/css; charset=utf-8", STYLE) }))
        .route("/logo.svg", get(|| async { asset("image/svg+xml", LOGO) }))
        .route("/fonts/{name}", get(font))
        .route("/session", post(sign_in).delete(sign_out).get(who_am_i))
        .route("/api/{*path}", any(proxy))
        .route("/events", get(crate::events::changes))
        .fallback(page)
        .with_state(state)
}
