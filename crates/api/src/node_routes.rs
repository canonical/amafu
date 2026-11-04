use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use amafu_domain::access::Verb;
use amafu_domain::registry::{self, KindDescriptor};
use amafu_domain::slice::NodeSlice;
use amafu_domain::{NodeName, ObjectId, ObjectName, ObjectReference, ProjectName};
use amafu_interfaces::node_lease::{
    acquire_node_lease, admit_node_write, lease_reference, node_fence, renew_node_lease,
};
use amafu_interfaces::{
    AuditEntry, AuditLog, AuditOutcome, Clock, Consistency, Identifiers, NodeLeaseError, NodeLeaseGrant,
    ObjectStoreError, SliceChange, SliceRecord, Store, StoreError, StoredRecord, WatchEvent,
};
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Extension, Request, State};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::stream::BoxStream;
use futures::{FutureExt, StreamExt};
use http::{HeaderValue, StatusCode};
use serde_json::Value;
use uuid::Uuid;

use crate::node_identity::{NodeAuthenticator, NodeCredentials, PeerCertificate, node_identity_of};
use crate::node_wire::{
    NODE_LEASE_ACQUIRE_PATH, NODE_LEASE_RENEW_PATH, NODE_LIST_PATH, NODE_READ_PATH, NODE_SUBSCRIPTION_PATH,
    NODE_SWAP_PATH, NodeLeaseAcquireRequest, NodeLeaseRenewRequest, NodeListRequest, NodeListResponse,
    NodeReadRequest, NodeReadResponse, NodeSwapRequest, NodeSwapResponse,
};
use crate::wire::{ErrorBody, ErrorCode, MAXIMUM_BODY_BYTES, REQUEST_ID_HEADER};

#[derive(Clone)]
pub struct NodeApiState {
    pub store: Arc<dyn Store>,
    pub authenticator: Arc<NodeAuthenticator>,
    pub audit: Arc<dyn AuditLog>,
    pub clock: Arc<dyn Clock>,
    pub identifiers: Arc<dyn Identifiers>,
    pub refresh_interval: Duration,
}

#[derive(Debug, Clone)]
struct NodeContext {
    request_id: Uuid,
    credentials: NodeCredentials,
    peer: PeerCertificate,
}

#[derive(Debug, Clone)]
struct NodeAudit {
    verb: Verb,
    reference: ObjectReference,
    is_denied: bool,
}

pub fn node_router(state: NodeApiState) -> Router {
    Router::new()
        .route(NODE_READ_PATH, post(read))
        .route(NODE_LIST_PATH, post(list))
        .route(NODE_SWAP_PATH, post(compare_and_swap))
        .route(NODE_SUBSCRIPTION_PATH, get(subscribe))
        .route(NODE_LEASE_ACQUIRE_PATH, post(acquire_lease))
        .route(NODE_LEASE_RENEW_PATH, post(renew_lease))
        .layer(DefaultBodyLimit::max(MAXIMUM_BODY_BYTES))
        .layer(middleware::from_fn_with_state(state.clone(), node_chain))
        .with_state(state)
}

fn principal_name(node: &str) -> String {
    format!("node:{node}")
}

fn error_response(status: StatusCode, code: ErrorCode, message: String, request_id: Uuid) -> Response {
    let body = ErrorBody { error: code, message, request_id, violations: Vec::new() };
    (status, Json(body)).into_response()
}

fn outcome(status: StatusCode, audit: Option<&NodeAudit>) -> AuditOutcome {
    if status == StatusCode::UNAUTHORIZED {
        AuditOutcome::Unauthenticated
    } else if audit.is_some_and(|audit| audit.is_denied) {
        AuditOutcome::Denied
    } else if status.is_success() {
        AuditOutcome::Succeeded { status: status.as_u16(), generation: None }
    } else {
        AuditOutcome::Rejected { status: status.as_u16() }
    }
}

async fn node_chain(State(state): State<NodeApiState>, mut request: Request, next: Next) -> Response {
    let request_id = state.identifiers.generate();
    let method = request.method().to_string();
    let path = request.uri().path().to_owned();
    let peer = request.extensions().get::<PeerCertificate>().cloned().unwrap_or_default();
    let claimed =
        peer.0.as_deref().and_then(|certificate| node_identity_of(certificate).ok()).map(|(name, _)| name);
    let (mut response, principal) = match state.authenticator.authenticate(&peer, state.clock.now()).await {
        Ok(credentials) => {
            let principal = principal_name(credentials.name.as_str());
            request.extensions_mut().insert(NodeContext { request_id, credentials, peer });
            (next.run(request).await, Some(principal))
        }
        Err(refusal) => {
            tracing::warn!(%request_id, %refusal, "a node was refused");
            let response = error_response(
                StatusCode::UNAUTHORIZED,
                ErrorCode::Unauthenticated,
                refusal.to_string(),
                request_id,
            );
            (response, claimed.map(|name| principal_name(name.as_str())))
        }
    };
    let audit = response.extensions_mut().remove::<NodeAudit>();
    let is_audited = audit.is_some() || !response.status().is_success();
    if is_audited {
        let entry = AuditEntry {
            request_id,
            recorded_at: state.clock.now(),
            principal,
            role: None,
            verb: audit.as_ref().map(|audit| audit.verb),
            kind: audit.as_ref().map(|audit| audit.reference.kind.clone()),
            project: audit.as_ref().and_then(|audit| audit.reference.project.clone()),
            name: audit.as_ref().map(|audit| audit.reference.name.clone()),
            method,
            path,
            outcome: outcome(response.status(), audit.as_ref()),
        };
        if let Err(error) = state.audit.record(entry).await {
            tracing::error!(%request_id, %error, "recording the audit entry failed");
            response = error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorCode::Unavailable,
                String::from("the request could not be audited"),
                request_id,
            );
        }
    }
    if let Ok(value) = HeaderValue::from_str(&request_id.to_string()) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    response
}

fn store_failure(error: &StoreError, request_id: Uuid) -> Response {
    match error {
        StoreError::Conflict => {
            error_response(StatusCode::CONFLICT, ErrorCode::Conflict, error.to_string(), request_id)
        }
        StoreError::Refused(_) | StoreError::Fenced { .. } => {
            error_response(StatusCode::FORBIDDEN, ErrorCode::Forbidden, error.to_string(), request_id)
        }
        StoreError::Unavailable(_) => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::Unavailable,
            error.to_string(),
            request_id,
        ),
        StoreError::Corrupt(_) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Internal,
            error.to_string(),
            request_id,
        ),
    }
}

fn refused(context: &NodeContext, verb: Verb, reference: ObjectReference, reason: &str) -> Response {
    let mut response =
        error_response(StatusCode::FORBIDDEN, ErrorCode::Forbidden, reason.to_owned(), context.request_id);
    response.extensions_mut().insert(NodeAudit { verb, reference, is_denied: true });
    response
}

async fn slice_of(store: &dyn Store, node: &NodeName) -> Result<NodeSlice, StoreError> {
    let vms = store.list("Vm", None, Consistency::Linearizable).await?;
    let mut slice = NodeSlice::new(node.clone());
    slice.learn_vms(vms.iter().map(|record| &record.object));
    Ok(slice)
}

fn reference_of(kind: &KindDescriptor, object: &Value) -> Option<ObjectReference> {
    let name = object.pointer("/metadata/name")?.as_str()?;
    let project = match object.pointer("/metadata/project").and_then(Value::as_str) {
        Some(project) if kind.is_project_scoped() => Some(ProjectName::new(project).ok()?),
        _ => None,
    };
    Some(ObjectReference { kind: kind.kind.to_owned(), project, name: ObjectName::new(name).ok()? })
}

async fn read(
    State(state): State<NodeApiState>,
    Extension(context): Extension<NodeContext>,
    Json(request): Json<NodeReadRequest>,
) -> Response {
    let store = state.store.as_ref();
    let outcome = async {
        let slice = slice_of(store, &context.credentials.name).await?;
        let record = store.read(&request.reference, Consistency::Linearizable).await?;
        Ok::<_, StoreError>((slice, record))
    }
    .await;
    match outcome {
        Err(error) => store_failure(&error, context.request_id),
        Ok((slice, Some(record))) if !slice.includes(&request.reference.kind, &record.object) => refused(
            &context,
            Verb::Get,
            request.reference,
            "the object is outside this node's share of the state",
        ),
        Ok((_, record)) => Json(NodeReadResponse { record }).into_response(),
    }
}

async fn list(
    State(state): State<NodeApiState>,
    Extension(context): Extension<NodeContext>,
    Json(request): Json<NodeListRequest>,
) -> Response {
    let store = state.store.as_ref();
    let outcome = async {
        let slice = slice_of(store, &context.credentials.name).await?;
        let records = store.list(&request.kind, request.project.as_ref(), Consistency::Linearizable).await?;
        Ok::<_, StoreError>(
            records.into_iter().filter(|record| slice.includes(&request.kind, &record.object)).collect(),
        )
    }
    .await;
    match outcome {
        Ok(records) => Json(NodeListResponse { records }).into_response(),
        Err(error) => store_failure(&error, context.request_id),
    }
}

fn names_match(reference: &ObjectReference, object: &Value) -> bool {
    object.pointer("/metadata/name").and_then(Value::as_str) == Some(reference.name.as_str())
        && object.pointer("/metadata/project").and_then(Value::as_str)
            == reference.project.as_ref().map(ProjectName::as_str)
}

async fn compare_and_swap(
    State(state): State<NodeApiState>,
    Extension(context): Extension<NodeContext>,
    Json(request): Json<NodeSwapRequest>,
) -> Response {
    let reference = request.reference.clone();
    let verb = match (&request.expected_revision, &request.replacement) {
        (None, _) => Verb::Create,
        (Some(_), Some(_)) => Verb::Update,
        (Some(_), None) => Verb::Delete,
    };
    let Some(kind) = registry::kind_named(&reference.kind) else {
        return refused(&context, verb, reference, "the kind is unknown");
    };
    if let Some(replacement) = &request.replacement
        && (!names_match(&reference, replacement) || (kind.check_object)(replacement).is_err())
    {
        return refused(
            &context,
            verb,
            reference,
            "the replacement is not a valid object of its kind and name",
        );
    }
    let store = state.store.as_ref();
    let outcome = async {
        let slice = slice_of(store, &context.credentials.name).await?;
        let current = store.read(&reference, Consistency::Linearizable).await?;
        Ok::<_, StoreError>((slice, current))
    }
    .await;
    let (slice, current) = match outcome {
        Ok(outcome) => outcome,
        Err(error) => return store_failure(&error, context.request_id),
    };
    let current_object = current.as_ref().map(|record| &record.object);
    if !slice.may_write(&context.credentials.roles, kind.kind, current_object, request.replacement.as_ref()) {
        return refused(&context, verb, reference, "a node may not make this change");
    }
    let node = &context.credentials.name;
    match admit_node_write(store, node, request.epoch).await {
        Ok(()) => {}
        Err(error) if error.is_refusal() => return refused(&context, verb, reference, &error.to_string()),
        Err(error) => return lease_failure(&error, context.request_id),
    }
    let fence = node_fence(node, request.epoch);
    let swapped = store
        .compare_and_swap_fenced(&reference, request.expected_revision, request.replacement, &fence)
        .await;
    let mut response = match swapped {
        Ok(revision) => Json(NodeSwapResponse { revision }).into_response(),
        Err(error) => store_failure(&error, context.request_id),
    };
    response.extensions_mut().insert(NodeAudit { verb, reference, is_denied: false });
    response
}

fn lease_failure(error: &NodeLeaseError, request_id: Uuid) -> Response {
    match error {
        NodeLeaseError::Store(ObjectStoreError::Store(store_error)) => store_failure(store_error, request_id),
        other => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::Unavailable,
            other.to_string(),
            request_id,
        ),
    }
}

fn lease_answer(
    context: &NodeContext,
    outcome: Result<NodeLeaseGrant, NodeLeaseError>,
    is_audited_on_success: bool,
) -> Response {
    let reference = lease_reference(&context.credentials.name);
    match outcome {
        Ok(grant) => {
            let mut response = Json(grant).into_response();
            if is_audited_on_success {
                response.extensions_mut().insert(NodeAudit {
                    verb: Verb::Update,
                    reference,
                    is_denied: false,
                });
            }
            response
        }
        Err(error) if error.is_refusal() => refused(context, Verb::Update, reference, &error.to_string()),
        Err(error) => lease_failure(&error, context.request_id),
    }
}

async fn acquire_lease(
    State(state): State<NodeApiState>,
    Extension(context): Extension<NodeContext>,
    Json(request): Json<NodeLeaseAcquireRequest>,
) -> Response {
    let id = ObjectId::from_uuid(state.identifiers.generate());
    let outcome = acquire_node_lease(
        state.store.as_ref(),
        &context.credentials.name,
        id,
        request.terms,
        state.clock.now(),
    )
    .await;
    if let Ok(grant) = &outcome {
        tracing::info!(node = %context.credentials.name, epoch = grant.epoch, "a node acquired its lease");
    }
    lease_answer(&context, outcome, true)
}

async fn renew_lease(
    State(state): State<NodeApiState>,
    Extension(context): Extension<NodeContext>,
    Json(request): Json<NodeLeaseRenewRequest>,
) -> Response {
    let outcome =
        renew_node_lease(state.store.as_ref(), &context.credentials.name, request.epoch, state.clock.now())
            .await;
    lease_answer(&context, outcome, false)
}

struct Subscription {
    state: NodeApiState,
    context: NodeContext,
    events: BoxStream<'static, WatchEvent>,
    sent: BTreeMap<ObjectReference, u64>,
    has_started: bool,
}

async fn current_view(
    state: &NodeApiState,
    node: &NodeName,
) -> Result<BTreeMap<ObjectReference, StoredRecord>, StoreError> {
    let store = state.store.as_ref();
    let slice = slice_of(store, node).await?;
    let mut view = BTreeMap::new();
    for kind in registry::kinds() {
        for record in store.list(kind.kind, None, Consistency::Linearizable).await? {
            if slice.includes(kind.kind, &record.object)
                && let Some(reference) = reference_of(&kind, &record.object)
            {
                view.insert(reference, record);
            }
        }
    }
    Ok(view)
}

impl Subscription {
    async fn wait_for_change(&mut self) -> bool {
        let refresh = self.state.clock.sleep(self.state.refresh_interval);
        let event = futures::future::select(self.events.next(), refresh).await;
        if let futures::future::Either::Left((None, _)) = event {
            return false;
        }
        while let Some(Some(_)) = self.events.next().now_or_never() {}
        true
    }

    async fn next_changes(&mut self) -> Option<Vec<SliceChange>> {
        if self.has_started && !self.wait_for_change().await {
            return None;
        }
        if let Err(refusal) =
            self.state.authenticator.authenticate(&self.context.peer, self.state.clock.now()).await
        {
            tracing::warn!(node = %self.context.credentials.name, %refusal, "ending a subscription");
            return None;
        }
        let Ok(view) = current_view(&self.state, &self.context.credentials.name).await else {
            return Some(vec![SliceChange::Heartbeat]);
        };
        let previous = std::mem::replace(
            &mut self.sent,
            view.iter().map(|(reference, record)| (reference.clone(), record.revision)).collect(),
        );
        if !self.has_started {
            self.has_started = true;
            let records =
                view.into_iter().map(|(reference, record)| SliceRecord { reference, record }).collect();
            return Some(vec![SliceChange::Snapshot { records }]);
        }
        let mut changes: Vec<SliceChange> = view
            .into_iter()
            .filter(|(reference, record)| previous.get(reference) != Some(&record.revision))
            .map(|(reference, record)| SliceChange::Upserted { record: SliceRecord { reference, record } })
            .collect();
        changes.extend(
            previous
                .into_keys()
                .filter(|reference| !self.sent.contains_key(reference))
                .map(|reference| SliceChange::Removed { reference }),
        );
        if changes.is_empty() {
            changes.push(SliceChange::Heartbeat);
        }
        Some(changes)
    }
}

fn encode_changes(changes: &[SliceChange]) -> Result<Bytes, std::io::Error> {
    let mut lines = Vec::new();
    for change in changes {
        lines.extend(serde_json::to_vec(change).map_err(std::io::Error::other)?);
        lines.push(b'\n');
    }
    Ok(Bytes::from(lines))
}

async fn subscribe(
    State(state): State<NodeApiState>,
    Extension(context): Extension<NodeContext>,
) -> Response {
    let events =
        futures::stream::select_all(registry::kinds().map(|kind| state.store.watch(kind.kind))).boxed();
    let subscription = Subscription { state, context, events, sent: BTreeMap::new(), has_started: false };
    let frames = futures::stream::unfold(subscription, |mut subscription| async move {
        let changes = subscription.next_changes().await?;
        Some((encode_changes(&changes), subscription))
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(http::header::CONTENT_TYPE, "application/x-ndjson")
        .body(Body::from_stream(frames))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}
