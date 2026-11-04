use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use amafu_domain::kinds::node_lease::LeaseTerms;
use amafu_domain::{ObjectReference, ProjectName};
use amafu_interfaces::{
    CoreConnection, NodeLeaseAuthority, NodeLeaseError, NodeLeaseGrant, SliceChange, SliceChanges,
    StoreError, StoredRecord,
};
use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use http::header::CONTENT_TYPE;
use http::{Method, Request, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use uuid::Uuid;

use crate::client::{ClientError, ResponseBody, Transport, collect, decode, refusal};
use crate::node_wire::{
    NODE_LEASE_ACQUIRE_PATH, NODE_LEASE_RENEW_PATH, NODE_LEASE_SURRENDER_PATH, NODE_LIST_PATH,
    NODE_READ_PATH, NODE_SUBSCRIPTION_PATH, NODE_SWAP_PATH, NodeLeaseAcquireRequest, NodeLeaseRenewRequest,
    NodeListRequest, NodeListResponse, NodeReadRequest, NodeReadResponse, NodeSwapRequest, NodeSwapResponse,
};
use crate::wire::REQUEST_ID_HEADER;

pub struct NodeClient {
    core_members: Vec<Arc<dyn Transport>>,
    preferred: AtomicUsize,
    last_request_id: Mutex<Option<Uuid>>,
}

#[allow(clippy::needless_pass_by_value)]
fn store_error(error: ClientError) -> StoreError {
    match error.status().and_then(|status| StatusCode::from_u16(status).ok()) {
        Some(StatusCode::CONFLICT) => StoreError::Conflict,
        Some(
            StatusCode::UNAUTHORIZED
            | StatusCode::FORBIDDEN
            | StatusCode::BAD_REQUEST
            | StatusCode::NOT_FOUND,
        ) => StoreError::Refused(error.to_string()),
        Some(StatusCode::INTERNAL_SERVER_ERROR) => StoreError::Corrupt(error.to_string()),
        _ => StoreError::Unavailable(error.to_string()),
    }
}

fn is_worth_another_member(error: &StoreError) -> bool {
    matches!(error, StoreError::Unavailable(_))
}

fn request(method: Method, path: &str, body: Option<Vec<u8>>) -> Result<Request<Bytes>, StoreError> {
    let mut builder = Request::builder().method(method).uri(path);
    if body.is_some() {
        builder = builder.header(CONTENT_TYPE, "application/json");
    }
    builder
        .body(body.map(Bytes::from).unwrap_or_default())
        .map_err(|error| StoreError::Unavailable(error.to_string()))
}

impl NodeClient {
    #[must_use]
    pub fn new(core_members: Vec<Arc<dyn Transport>>) -> Self {
        Self { core_members, preferred: AtomicUsize::new(0), last_request_id: Mutex::new(None) }
    }

    #[must_use]
    pub fn last_request_id(&self) -> Option<Uuid> {
        *self.last_request_id.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn members_in_order(&self) -> impl Iterator<Item = (usize, &Arc<dyn Transport>)> {
        let count = self.core_members.len();
        let preferred = self.preferred.load(Ordering::Relaxed);
        (0..count)
            .map(move |offset| (preferred + offset) % count)
            .map(|index| (index, &self.core_members[index]))
    }

    async fn exchange(
        &self,
        transport: &dyn Transport,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<ResponseBody, StoreError> {
        let response = transport.send(request(method, path, body)?).await.map_err(store_error)?;
        let request_id = response
            .headers()
            .get(REQUEST_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| Uuid::parse_str(value).ok());
        *self.last_request_id.lock().unwrap_or_else(PoisonError::into_inner) = request_id;
        let status = response.status().as_u16();
        if status != StatusCode::OK.as_u16() {
            let body = collect(response.into_body()).await.map_err(store_error)?;
            return Err(store_error(refusal(status, &body)));
        }
        Ok(response.into_body())
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<ResponseBody, StoreError> {
        let mut last_error = StoreError::Unavailable(String::from("no core member is configured"));
        for (index, transport) in self.members_in_order() {
            match self.exchange(transport.as_ref(), method.clone(), path, body.clone()).await {
                Ok(response) => {
                    self.preferred.store(index, Ordering::Relaxed);
                    return Ok(response);
                }
                Err(error) if is_worth_another_member(&error) => last_error = error,
                Err(error) => return Err(error),
            }
        }
        Err(last_error)
    }

    async fn post<Body: Serialize, Answer: DeserializeOwned>(
        &self,
        path: &str,
        body: &Body,
    ) -> Result<Answer, StoreError> {
        let encoded = serde_json::to_vec(body).map_err(|error| StoreError::Corrupt(error.to_string()))?;
        let response = self.send(Method::POST, path, Some(encoded)).await?;
        let bytes = collect(response).await.map_err(store_error)?;
        decode(&bytes).map_err(store_error)
    }
}

fn slice_changes(body: ResponseBody) -> SliceChanges {
    futures::stream::unfold((body, BytesMut::new()), |(mut body, mut buffer)| async move {
        loop {
            if let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
                let line = buffer.split_to(position + 1);
                let change = decode::<SliceChange>(&line[..position]).map_err(store_error);
                return Some((change, (body, buffer)));
            }
            match body.next().await? {
                Ok(chunk) => buffer.extend_from_slice(&chunk),
                Err(error) => return Some((Err(store_error(error)), (body, buffer))),
            }
        }
    })
    .boxed()
}

#[async_trait::async_trait]
impl CoreConnection for NodeClient {
    async fn read(&self, reference: &ObjectReference) -> Result<Option<StoredRecord>, StoreError> {
        let response: NodeReadResponse =
            self.post(NODE_READ_PATH, &NodeReadRequest { reference: reference.clone() }).await?;
        Ok(response.record)
    }

    async fn list(&self, kind: &str, project: Option<&ProjectName>) -> Result<Vec<StoredRecord>, StoreError> {
        let request = NodeListRequest { kind: kind.to_owned(), project: project.cloned() };
        let response: NodeListResponse = self.post(NODE_LIST_PATH, &request).await?;
        Ok(response.records)
    }

    async fn compare_and_swap(
        &self,
        reference: &ObjectReference,
        expected_revision: Option<u64>,
        replacement: Option<Value>,
        epoch: u64,
    ) -> Result<u64, StoreError> {
        let request = NodeSwapRequest { reference: reference.clone(), expected_revision, replacement, epoch };
        let response: NodeSwapResponse = self.post(NODE_SWAP_PATH, &request).await?;
        Ok(response.revision)
    }

    async fn subscribe(&self) -> Result<SliceChanges, StoreError> {
        let body = self.send(Method::GET, NODE_SUBSCRIPTION_PATH, None).await?;
        Ok(slice_changes(body))
    }
}

#[async_trait::async_trait]
impl NodeLeaseAuthority for NodeClient {
    async fn acquire(&self, terms: LeaseTerms) -> Result<NodeLeaseGrant, NodeLeaseError> {
        Ok(self.post(NODE_LEASE_ACQUIRE_PATH, &NodeLeaseAcquireRequest { terms }).await?)
    }

    async fn renew(&self, epoch: u64) -> Result<NodeLeaseGrant, NodeLeaseError> {
        Ok(self.post(NODE_LEASE_RENEW_PATH, &NodeLeaseRenewRequest { epoch }).await?)
    }

    async fn surrender(&self, epoch: u64) -> Result<(), NodeLeaseError> {
        let _: NodeLeaseGrant =
            self.post(NODE_LEASE_SURRENDER_PATH, &NodeLeaseRenewRequest { epoch }).await?;
        Ok(())
    }
}
