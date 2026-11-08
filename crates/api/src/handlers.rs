use std::sync::Arc;

use amafu_domain::access::Verb;
use amafu_domain::registry::{KindDescriptor, UntypedChange};
use amafu_domain::{ChangeError, ObjectId, ObjectName, ObjectReference, ProjectName, Violation};
use amafu_interfaces::objects::{ObjectStoreError, apply_untyped};
use amafu_interfaces::{Consistency, Store, StoreError, WatchEvent};
use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::Query;
use axum::response::{IntoResponse, Response};
use futures::{Stream, StreamExt};
use http::{StatusCode, Uri, header};
use serde_json::Value;
use uuid::Uuid;

use crate::admission::{AdmissionError, admit_create, admit_update};
use crate::authorization::{Target, is_allowed};
use crate::request_chain::{AuditDetails, RequestContext};
use crate::routes::ApiState;
use crate::wire::{CollectionFrame, ErrorBody, ErrorCode, ListResponse, ProjectQuery, WatchFrame};

pub struct ApiError {
    status: StatusCode,
    code: ErrorCode,
    message: String,
    violations: Vec<Violation>,
    request_id: Uuid,
    audit: Box<AuditDetails>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorBody {
            error: self.code,
            message: self.message,
            request_id: self.request_id,
            violations: self.violations,
        };
        let mut response = (self.status, Json(body)).into_response();
        response.extensions_mut().insert(*self.audit);
        response
    }
}

pub struct ApiSuccess {
    status: StatusCode,
    body: Body,
    content_type: &'static str,
    audit: AuditDetails,
}

impl IntoResponse for ApiSuccess {
    fn into_response(self) -> Response {
        let mut response =
            (self.status, [(header::CONTENT_TYPE, self.content_type)], self.body).into_response();
        response.extensions_mut().insert(self.audit);
        response
    }
}

struct Operation {
    kind: KindDescriptor,
    verb: Verb,
    context: RequestContext,
    project: Option<ProjectName>,
    is_every_project: bool,
    name: Option<ObjectName>,
}

impl Operation {
    fn new(
        kind: KindDescriptor,
        verb: Verb,
        context: RequestContext,
        target: &Uri,
        name: Option<String>,
    ) -> Result<Self, ApiError> {
        let mut operation = Self { kind, verb, context, project: None, is_every_project: false, name: None };
        let query = Query::<ProjectQuery>::try_from_uri(target)
            .map_err(|error| {
                operation.error(StatusCode::BAD_REQUEST, ErrorCode::Malformed, error.to_string())
            })?
            .0;
        if kind.is_project_scoped() {
            if query.all_projects {
                let is_collection_read = verb == Verb::List || (verb == Verb::Watch && name.is_none());
                if !is_collection_read {
                    return Err(operation.error(
                        StatusCode::BAD_REQUEST,
                        ErrorCode::Malformed,
                        String::from("all-projects applies only to listing and watching"),
                    ));
                }
                operation.is_every_project = true;
            } else {
                operation.project =
                    Some(query.project.unwrap_or_else(|| operation.context.principal.project.clone()));
            }
        }
        if let Some(name) = name {
            operation.name = Some(ObjectName::new(name).map_err(|error| {
                operation.error(StatusCode::NOT_FOUND, ErrorCode::NotFound, error.to_string())
            })?);
        }
        Ok(operation)
    }

    fn audit(&self) -> AuditDetails {
        AuditDetails {
            verb: Some(self.verb),
            kind: Some(self.kind.kind),
            project: self.project.clone(),
            name: self.name.clone(),
            is_denied: false,
            generation: None,
        }
    }

    fn error(&self, status: StatusCode, code: ErrorCode, message: String) -> ApiError {
        ApiError {
            status,
            code,
            message,
            violations: Vec::new(),
            request_id: self.context.request_id,
            audit: Box::new(self.audit()),
        }
    }

    fn target(&self) -> Target<'_> {
        match (&self.project, self.is_every_project) {
            (_, true) => Target::EveryProject,
            (Some(project), false) => Target::Project(project),
            (None, false) => Target::Cluster,
        }
    }

    fn authorize(&self) -> Result<(), ApiError> {
        if is_allowed(&self.context.principal, self.verb, &self.kind, self.target()) {
            return Ok(());
        }
        let mut error = self.error(
            StatusCode::FORBIDDEN,
            ErrorCode::Forbidden,
            format!("{} may not {} {} here", self.context.principal.role, self.verb, self.kind.plural_name),
        );
        error.audit.is_denied = true;
        Err(error)
    }

    fn reference(&self) -> Result<ObjectReference, ApiError> {
        let name = self.name.clone().ok_or_else(|| {
            self.error(StatusCode::NOT_FOUND, ErrorCode::NotFound, String::from("no object name"))
        })?;
        Ok(ObjectReference { kind: self.kind.kind.to_owned(), project: self.project.clone(), name })
    }

    fn store_error(&self, error: ObjectStoreError) -> ApiError {
        let (status, code) = match &error {
            ObjectStoreError::Change(ChangeError::NotFound) => (StatusCode::NOT_FOUND, ErrorCode::NotFound),
            ObjectStoreError::Change(ChangeError::AlreadyExists) => {
                (StatusCode::CONFLICT, ErrorCode::AlreadyExists)
            }
            ObjectStoreError::Change(ChangeError::Invalid(_)) => {
                (StatusCode::UNPROCESSABLE_ENTITY, ErrorCode::Invalid)
            }
            ObjectStoreError::Change(_) => (StatusCode::CONFLICT, ErrorCode::Conflict),
            ObjectStoreError::Malformed(_) => (StatusCode::BAD_REQUEST, ErrorCode::Malformed),
            ObjectStoreError::Store(
                StoreError::Unavailable(_) | StoreError::Conflict | StoreError::Fenced { .. },
            )
            | ObjectStoreError::Contended => (StatusCode::SERVICE_UNAVAILABLE, ErrorCode::Unavailable),
            ObjectStoreError::Store(StoreError::Refused(_)) => (StatusCode::FORBIDDEN, ErrorCode::Forbidden),
            ObjectStoreError::Store(StoreError::Corrupt(_)) | ObjectStoreError::Corrupt(_) => {
                tracing::error!(%error, "stored data is corrupt");
                (StatusCode::INTERNAL_SERVER_ERROR, ErrorCode::Internal)
            }
        };
        let mut api_error = self.error(status, code, error.to_string());
        if let ObjectStoreError::Change(ChangeError::Invalid(validation)) = error {
            api_error.violations = validation.violations;
        }
        api_error
    }

    fn admission_error(&self, error: AdmissionError) -> ApiError {
        match error {
            AdmissionError::Malformed(message) => {
                self.error(StatusCode::BAD_REQUEST, ErrorCode::Malformed, message)
            }
            AdmissionError::Invalid(validation) => {
                let mut api_error =
                    self.error(StatusCode::UNPROCESSABLE_ENTITY, ErrorCode::Invalid, validation.to_string());
                api_error.violations = validation.violations;
                api_error
            }
        }
    }

    fn json(&self, status: StatusCode, value: &impl serde::Serialize) -> Result<ApiSuccess, ApiError> {
        let body = serde_json::to_vec(value).map_err(|error| {
            self.error(StatusCode::INTERNAL_SERVER_ERROR, ErrorCode::Internal, error.to_string())
        })?;
        Ok(ApiSuccess {
            status,
            body: Body::from(body),
            content_type: "application/json",
            audit: self.audit(),
        })
    }

    fn json_with_generation(&self, status: StatusCode, object: &Value) -> Result<ApiSuccess, ApiError> {
        let mut success = self.json(status, object)?;
        success.audit.generation = object["metadata"]["generation"].as_u64();
        Ok(success)
    }
}

const LOCAL_CONSISTENCY: &str = "consistency=local";
const WATCH_REQUEST: &str = "watch=true";

fn has_query_pair(target: &Uri, expected: &str) -> bool {
    target.query().is_some_and(|query| query.split('&').any(|pair| pair == expected))
}

fn requested_consistency(target: &Uri) -> Consistency {
    if has_query_pair(target, LOCAL_CONSISTENCY) { Consistency::Stale } else { Consistency::Linearizable }
}

pub async fn list_objects(
    kind: KindDescriptor,
    state: ApiState,
    context: RequestContext,
    target: Uri,
) -> Result<ApiSuccess, ApiError> {
    if has_query_pair(&target, WATCH_REQUEST) {
        return watch_collection(kind, state, context, target).await;
    }
    let operation = Operation::new(kind, Verb::List, context, &target, None)?;
    operation.authorize()?;
    let records = state
        .store
        .list(kind.kind, operation.project.as_ref(), requested_consistency(&target))
        .await
        .map_err(|error| operation.store_error(error.into()))?;
    let items = records.into_iter().map(|record| record.object).collect();
    operation.json(StatusCode::OK, &ListResponse { items })
}

pub async fn get_object(
    kind: KindDescriptor,
    state: ApiState,
    context: RequestContext,
    target: Uri,
    name: String,
) -> Result<ApiSuccess, ApiError> {
    let operation = Operation::new(kind, Verb::Get, context, &target, Some(name))?;
    operation.authorize()?;
    let reference = operation.reference()?;
    let record = state
        .store
        .read(&reference, requested_consistency(&target))
        .await
        .map_err(|error| operation.store_error(error.into()))?
        .ok_or_else(|| operation.store_error(ChangeError::NotFound.into()))?;
    operation.json(StatusCode::OK, &record.object)
}

pub async fn create_object(
    kind: KindDescriptor,
    state: ApiState,
    context: RequestContext,
    target: Uri,
    body: Bytes,
) -> Result<ApiSuccess, ApiError> {
    let mut operation = Operation::new(kind, Verb::Create, context, &target, None)?;
    operation.authorize()?;
    let id = ObjectId::from_uuid(state.identifiers.generate());
    let change = admit_create(&kind, operation.project.clone(), &body, id)
        .map_err(|error| operation.admission_error(error))?;
    if let UntypedChange::Create { name, .. } = &change {
        operation.name = Some(name.clone());
    }
    let reference = operation.reference()?;
    let created = apply_untyped(state.store.as_ref(), &kind, &reference, change)
        .await
        .map_err(|error| operation.store_error(error))?;
    operation.json_with_generation(StatusCode::CREATED, &created)
}

pub async fn update_object(
    kind: KindDescriptor,
    state: ApiState,
    context: RequestContext,
    target: Uri,
    name: String,
    body: Bytes,
) -> Result<ApiSuccess, ApiError> {
    let operation = Operation::new(kind, Verb::Update, context, &target, Some(name))?;
    operation.authorize()?;
    let reference = operation.reference()?;
    let change = admit_update(&body).map_err(|error| operation.admission_error(error))?;
    let updated = apply_untyped(state.store.as_ref(), &kind, &reference, change)
        .await
        .map_err(|error| operation.store_error(error))?;
    operation.json_with_generation(StatusCode::OK, &updated)
}

pub async fn delete_object(
    kind: KindDescriptor,
    state: ApiState,
    context: RequestContext,
    target: Uri,
    name: String,
) -> Result<ApiSuccess, ApiError> {
    let operation = Operation::new(kind, Verb::Delete, context, &target, Some(name))?;
    operation.authorize()?;
    let reference = operation.reference()?;
    let deleting = apply_untyped(state.store.as_ref(), &kind, &reference, UntypedChange::Delete)
        .await
        .map_err(|error| operation.store_error(error))?;
    operation.json_with_generation(StatusCode::ACCEPTED, &deleting)
}

pub async fn watch_object(
    kind: KindDescriptor,
    state: ApiState,
    context: RequestContext,
    target: Uri,
    name: String,
) -> Result<ApiSuccess, ApiError> {
    let operation = Operation::new(kind, Verb::Watch, context, &target, Some(name))?;
    operation.authorize()?;
    let reference = operation.reference()?;
    let events = state.store.watch(kind.kind);
    let initial = state
        .store
        .read(&reference, Consistency::Linearizable)
        .await
        .map_err(|error| operation.store_error(error.into()))?
        .ok_or_else(|| operation.store_error(ChangeError::NotFound.into()))?;
    let frames = watch_frames(Arc::clone(&state.store), reference, initial.revision, initial.object, events);
    Ok(ApiSuccess {
        status: StatusCode::OK,
        body: Body::from_stream(frames),
        content_type: "application/x-ndjson",
        audit: operation.audit(),
    })
}

async fn watch_collection(
    kind: KindDescriptor,
    state: ApiState,
    context: RequestContext,
    target: Uri,
) -> Result<ApiSuccess, ApiError> {
    let operation = Operation::new(kind, Verb::Watch, context, &target, None)?;
    operation.authorize()?;
    let events = state.store.watch(kind.kind);
    let items =
        snapshot(state.store.as_ref(), kind.kind, operation.project.as_ref(), requested_consistency(&target))
            .await
            .map_err(|error| operation.store_error(error.into()))?;
    let progress = CollectionProgress {
        source: CollectionSource {
            store: Arc::clone(&state.store),
            kind: kind.kind,
            project: operation.project.clone(),
            is_every_project: operation.is_every_project,
        },
        pending: Some(CollectionFrame::Snapshot { items }),
        events,
    };
    Ok(ApiSuccess {
        status: StatusCode::OK,
        body: Body::from_stream(collection_frames(progress)),
        content_type: "application/x-ndjson",
        audit: operation.audit(),
    })
}

async fn snapshot(
    store: &dyn Store,
    kind: &str,
    project: Option<&ProjectName>,
    consistency: Consistency,
) -> Result<Vec<Value>, StoreError> {
    Ok(store.list(kind, project, consistency).await?.into_iter().map(|record| record.object).collect())
}

struct CollectionSource {
    store: Arc<dyn Store>,
    kind: &'static str,
    project: Option<ProjectName>,
    is_every_project: bool,
}

struct CollectionProgress {
    source: CollectionSource,
    pending: Option<CollectionFrame>,
    events: futures::stream::BoxStream<'static, WatchEvent>,
}

impl CollectionSource {
    fn includes(&self, project: Option<&ProjectName>) -> bool {
        self.is_every_project || self.project.as_ref() == project
    }

    async fn frame_for(&self, event: WatchEvent) -> Result<Option<CollectionFrame>, StoreError> {
        match event {
            WatchEvent::Resynchronize => {
                let items = snapshot(
                    self.store.as_ref(),
                    self.kind,
                    self.project.as_ref(),
                    Consistency::Linearizable,
                )
                .await?;
                Ok(Some(CollectionFrame::Snapshot { items }))
            }
            WatchEvent::Changed(reference) if self.includes(reference.project.as_ref()) => {
                Ok(Some(match self.store.read(&reference, Consistency::Linearizable).await? {
                    Some(record) => CollectionFrame::Changed { object: record.object },
                    None => CollectionFrame::Removed { project: reference.project, name: reference.name },
                }))
            }
            WatchEvent::Changed(_) => Ok(None),
        }
    }
}

fn encode_line(frame: &impl serde::Serialize) -> Result<Bytes, std::io::Error> {
    let mut line = serde_json::to_vec(frame).map_err(std::io::Error::other)?;
    line.push(b'\n');
    Ok(Bytes::from(line))
}

fn collection_frames(
    progress: CollectionProgress,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send {
    futures::stream::unfold(progress, |mut progress| async move {
        loop {
            if let Some(frame) = progress.pending.take() {
                return Some((encode_line(&frame), progress));
            }
            let event = progress.events.next().await?;
            match progress.source.frame_for(event).await {
                Ok(frame) => progress.pending = frame,
                Err(error) => return Some((Err(std::io::Error::other(error)), progress)),
            }
        }
    })
}

struct WatchProgress {
    store: Arc<dyn Store>,
    reference: ObjectReference,
    last_revision: Option<u64>,
    pending: Option<WatchFrame>,
    events: futures::stream::BoxStream<'static, WatchEvent>,
}

fn watch_frames(
    store: Arc<dyn Store>,
    reference: ObjectReference,
    revision: u64,
    object: Value,
    events: futures::stream::BoxStream<'static, WatchEvent>,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send {
    let progress = WatchProgress {
        store,
        reference,
        last_revision: Some(revision),
        pending: Some(WatchFrame::Object { object }),
        events,
    };
    futures::stream::unfold(progress, |mut progress| async move {
        loop {
            if let Some(frame) = progress.pending.take() {
                return Some((encode_line(&frame), progress));
            }
            progress.last_revision?;
            let event = progress.events.next().await?;
            let is_relevant = match &event {
                WatchEvent::Changed(changed) => *changed == progress.reference,
                WatchEvent::Resynchronize => true,
            };
            if !is_relevant {
                continue;
            }
            match progress.store.read(&progress.reference, Consistency::Linearizable).await {
                Ok(Some(record)) if Some(record.revision) != progress.last_revision => {
                    progress.last_revision = Some(record.revision);
                    progress.pending = Some(WatchFrame::Object { object: record.object });
                }
                Ok(Some(_)) => {}
                Ok(None) => {
                    progress.last_revision = None;
                    progress.pending = Some(WatchFrame::Removed);
                }
                Err(error) => return Some((Err(std::io::Error::other(error)), progress)),
            }
        }
    })
}
