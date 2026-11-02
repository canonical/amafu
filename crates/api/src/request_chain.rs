use amafu_domain::access::Verb;
use amafu_domain::{ObjectName, ProjectName};
use amafu_interfaces::{AuditEntry, AuditOutcome};
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http::{HeaderValue, StatusCode};
use uuid::Uuid;

use crate::identity::Principal;
use crate::routes::ApiState;
use crate::wire::{ErrorBody, ErrorCode, REQUEST_ID_HEADER};

#[derive(Debug, Clone)]
pub struct RequestContext {
    pub request_id: Uuid,
    pub principal: Principal,
}

#[derive(Debug, Clone, Default)]
pub struct AuditDetails {
    pub verb: Option<Verb>,
    pub kind: Option<&'static str>,
    pub project: Option<ProjectName>,
    pub name: Option<ObjectName>,
    pub is_denied: bool,
    pub generation: Option<u64>,
}

fn outcome_of(status: StatusCode, details: &AuditDetails) -> AuditOutcome {
    if status == StatusCode::UNAUTHORIZED {
        AuditOutcome::Unauthenticated
    } else if details.is_denied {
        AuditOutcome::Denied
    } else if status.is_success() {
        AuditOutcome::Succeeded { status: status.as_u16(), generation: details.generation }
    } else {
        AuditOutcome::Rejected { status: status.as_u16() }
    }
}

fn unauthenticated(request_id: Uuid, message: String) -> Response {
    let body = ErrorBody { error: ErrorCode::Unauthenticated, message, request_id, violations: Vec::new() };
    (StatusCode::UNAUTHORIZED, axum::Json(body)).into_response()
}

fn audit_failure(request_id: Uuid) -> Response {
    let body = ErrorBody {
        error: ErrorCode::Unavailable,
        message: String::from("the request could not be audited"),
        request_id,
        violations: Vec::new(),
    };
    (StatusCode::SERVICE_UNAVAILABLE, axum::Json(body)).into_response()
}

pub async fn request_chain(State(state): State<ApiState>, mut request: Request, next: Next) -> Response {
    let request_id = state.identifiers.generate();
    let method = request.method().to_string();
    let path = request.uri().path().to_owned();
    let authenticated = state.authenticator.authenticate(request.headers(), state.clock.now());
    let principal = authenticated.as_ref().ok().cloned();
    let mut response = match authenticated {
        Ok(principal) => {
            request.extensions_mut().insert(RequestContext { request_id, principal });
            next.run(request).await
        }
        Err(error) => unauthenticated(request_id, error.to_string()),
    };
    let details = response.extensions_mut().remove::<AuditDetails>().unwrap_or_default();
    let entry = AuditEntry {
        request_id,
        recorded_at: state.clock.now(),
        principal: principal.as_ref().map(|principal| principal.name.clone()),
        role: principal.as_ref().map(|principal| principal.role),
        verb: details.verb,
        kind: details.kind.map(str::to_owned),
        project: details.project.clone(),
        name: details.name.clone(),
        method,
        path,
        outcome: outcome_of(response.status(), &details),
    };
    if let Err(error) = state.audit.record(entry).await {
        tracing::error!(%request_id, %error, "recording the audit entry failed");
        response = audit_failure(request_id);
    }
    if let Ok(value) = HeaderValue::from_str(&request_id.to_string()) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    response
}
