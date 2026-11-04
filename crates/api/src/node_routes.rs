use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use amafu_domain::access::Verb;
use amafu_domain::registry::{self, KindDescriptor};
use amafu_domain::slice::{NodeSlice, SUBSCRIBED_KINDS, depends_on_hosted_vms};
use amafu_domain::{NodeName, ObjectId, ObjectName, ObjectReference, ProjectName};
use amafu_interfaces::node_lease::{
    acquire_node_lease, admit_node_write, lease_reference, node_fence, renew_node_lease, surrender_node_lease,
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
    NODE_LEASE_ACQUIRE_PATH, NODE_LEASE_RENEW_PATH, NODE_LEASE_SURRENDER_PATH, NODE_LIST_PATH,
    NODE_READ_PATH, NODE_SUBSCRIPTION_PATH, NODE_SWAP_PATH, NodeLeaseAcquireRequest, NodeLeaseRenewRequest,
    NodeListRequest, NodeListResponse, NodeReadRequest, NodeReadResponse, NodeSwapRequest, NodeSwapResponse,
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
        .route(NODE_LEASE_SURRENDER_PATH, post(surrender_lease))
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
    let vms = store.list("Vm", None, Consistency::Stale).await?;
    let mut slice = NodeSlice::new(node.clone());
    slice.learn_vms(vms.iter().map(|record| &record.object));
    Ok(slice)
}

async fn slice_for(store: &dyn Store, node: &NodeName, kind: &str) -> Result<NodeSlice, StoreError> {
    if depends_on_hosted_vms(kind) { slice_of(store, node).await } else { Ok(NodeSlice::new(node.clone())) }
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
        let slice = slice_for(store, &context.credentials.name, &request.reference.kind).await?;
        let record = store.read(&request.reference, Consistency::Stale).await?;
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
        let slice = slice_for(store, &context.credentials.name, &request.kind).await?;
        let records = store.list(&request.kind, request.project.as_ref(), Consistency::Stale).await?;
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
        let slice = slice_for(store, &context.credentials.name, &reference.kind).await?;
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

async fn surrender_lease(
    State(state): State<NodeApiState>,
    Extension(context): Extension<NodeContext>,
    Json(request): Json<NodeLeaseRenewRequest>,
) -> Response {
    let outcome = surrender_node_lease(state.store.as_ref(), &context.credentials.name, request.epoch).await;
    if let Ok(grant) = &outcome {
        tracing::info!(node = %context.credentials.name, epoch = grant.epoch, "a node surrendered its lease");
    }
    lease_answer(&context, outcome, true)
}

const REFRESHES_PER_FULL_VIEW: u32 = 12;

struct Subscription {
    state: NodeApiState,
    context: NodeContext,
    events: BoxStream<'static, WatchEvent>,
    sent: BTreeMap<ObjectReference, u64>,
    slice: NodeSlice,
    has_started: bool,
    refreshes_since_full_view: u32,
}

enum Wake {
    Changed(BTreeSet<ObjectReference>),
    Refresh,
    Resynchronize,
}

async fn current_view(
    store: &dyn Store,
    slice: &NodeSlice,
) -> Result<BTreeMap<ObjectReference, StoredRecord>, StoreError> {
    let mut view = BTreeMap::new();
    for kind in registry::kinds().iter().filter(|kind| SUBSCRIBED_KINDS.contains(&kind.kind)) {
        for record in store.list(kind.kind, None, Consistency::Stale).await? {
            if slice.includes(kind.kind, &record.object)
                && let Some(reference) = reference_of(kind, &record.object)
            {
                view.insert(reference, record);
            }
        }
    }
    Ok(view)
}

async fn is_still_trusted(state: &NodeApiState, context: &NodeContext) -> bool {
    match state.authenticator.authenticate(&context.peer, state.clock.now()).await {
        Ok(_) => true,
        Err(refusal) => {
            tracing::warn!(node = %context.credentials.name, %refusal, "ending a subscription");
            false
        }
    }
}

struct NodeShare<'share> {
    store: &'share dyn Store,
    node: &'share NodeName,
    slice: &'share NodeSlice,
    sent: &'share BTreeMap<ObjectReference, u64>,
}

impl NodeShare<'_> {
    async fn is_concerned_by(&self, reference: &ObjectReference) -> bool {
        match reference.kind.as_str() {
            "Node" => reference.name.as_str() == self.node.as_str(),
            "Vm" => {
                self.sent.contains_key(reference)
                    || matches!(
                        self.store.read(reference, Consistency::Stale).await,
                        Ok(Some(record)) if self.slice.includes("Vm", &record.object)
                    )
            }
            _ => true,
        }
    }
}

impl Subscription {
    async fn wait_for_change(&mut self) -> Option<Wake> {
        let mut refresh = self.state.clock.sleep(self.state.refresh_interval);
        let mut changed = BTreeSet::new();
        loop {
            let next = futures::future::select(self.events.next(), &mut refresh).await;
            let first = match next {
                futures::future::Either::Right(((), _)) => return Some(Wake::Refresh),
                futures::future::Either::Left((None, _)) => return None,
                futures::future::Either::Left((Some(event), _)) => event,
            };
            let mut pending = vec![first];
            while let Some(Some(event)) = self.events.next().now_or_never() {
                pending.push(event);
            }
            let share = NodeShare {
                store: self.state.store.as_ref(),
                node: &self.context.credentials.name,
                slice: &self.slice,
                sent: &self.sent,
            };
            for event in pending {
                match event {
                    WatchEvent::Resynchronize => return Some(Wake::Resynchronize),
                    WatchEvent::Changed(reference) if share.is_concerned_by(&reference).await => {
                        changed.insert(reference);
                    }
                    WatchEvent::Changed(_) => {}
                }
            }
            if !changed.is_empty() {
                return Some(Wake::Changed(changed));
            }
        }
    }

    async fn full_view(&mut self) -> Result<Vec<SliceChange>, StoreError> {
        let store = self.state.store.as_ref();
        self.slice = slice_of(store, &self.context.credentials.name).await?;
        let view = current_view(store, &self.slice).await?;
        self.refreshes_since_full_view = 0;
        let previous = std::mem::replace(
            &mut self.sent,
            view.iter().map(|(reference, record)| (reference.clone(), record.revision)).collect(),
        );
        if !self.has_started {
            self.has_started = true;
            let records =
                view.into_iter().map(|(reference, record)| SliceRecord { reference, record }).collect();
            return Ok(vec![SliceChange::Snapshot { records }]);
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
        Ok(changes)
    }

    fn changes_the_slice(&self, reference: &ObjectReference, record: Option<&StoredRecord>) -> bool {
        if reference.kind != "Vm" {
            return false;
        }
        let was_sent = self.sent.contains_key(reference);
        let included = record.filter(|record| self.slice.includes("Vm", &record.object));
        match included {
            Some(record) => !self.slice.accounts_for(&record.object),
            None => was_sent,
        }
    }

    async fn changes_to(
        &mut self,
        changed: BTreeSet<ObjectReference>,
    ) -> Result<Vec<SliceChange>, StoreError> {
        let mut records = Vec::with_capacity(changed.len());
        for reference in changed {
            let record = self.state.store.read(&reference, Consistency::Stale).await?;
            if self.changes_the_slice(&reference, record.as_ref()) {
                return self.full_view().await;
            }
            records.push((reference, record));
        }
        let mut slice_changes = Vec::new();
        for (reference, record) in records {
            let included = record.filter(|record| self.slice.includes(&reference.kind, &record.object));
            if let Some(record) = included {
                if self.sent.insert(reference.clone(), record.revision) != Some(record.revision) {
                    slice_changes.push(SliceChange::Upserted { record: SliceRecord { reference, record } });
                }
            } else if self.sent.remove(&reference).is_some() {
                slice_changes.push(SliceChange::Removed { reference });
            }
        }
        Ok(slice_changes)
    }

    async fn next_changes(&mut self) -> Option<Vec<SliceChange>> {
        loop {
            let wake = if self.has_started { self.wait_for_change().await? } else { Wake::Resynchronize };
            if !is_still_trusted(&self.state, &self.context).await {
                return None;
            }
            let is_change = matches!(wake, Wake::Changed(_));
            let computed = match wake {
                Wake::Changed(changed) => self.changes_to(changed).await,
                Wake::Resynchronize => self.full_view().await,
                Wake::Refresh => {
                    self.refreshes_since_full_view += 1;
                    if self.refreshes_since_full_view >= REFRESHES_PER_FULL_VIEW {
                        self.full_view().await
                    } else {
                        Ok(Vec::new())
                    }
                }
            };
            match computed {
                Ok(changes) if !changes.is_empty() => return Some(changes),
                Ok(_) if is_change => {}
                Ok(_) | Err(_) => return Some(vec![SliceChange::Heartbeat]),
            }
        }
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
        futures::stream::select_all(SUBSCRIBED_KINDS.iter().map(|kind| state.store.watch(kind))).boxed();
    let slice = NodeSlice::new(context.credentials.name.clone());
    let subscription = Subscription {
        state,
        context,
        events,
        sent: BTreeMap::new(),
        slice,
        has_started: false,
        refreshes_since_full_view: 0,
    };
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
