use std::collections::BTreeSet;
use std::convert::Infallible;

use amafu_api::client::ResponseBody;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use bytes::{Bytes, BytesMut};
use futures::stream::{BoxStream, StreamExt};
use serde::Deserialize;

use crate::server::{DashboardState, current_session, refusal, upstream_request};

const LARGEST_SUBSCRIPTION: usize = 32;
const EVERY_PROJECT: &str = "*";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Collection {
    pub key: String,
    pub path: String,
}

fn is_name(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-')
}

#[must_use]
pub fn collection(key: &str) -> Option<Collection> {
    let (plural, scope) = match key.split_once('@') {
        Some((plural, scope)) => (plural, Some(scope)),
        None => (key, None),
    };
    if !is_name(plural) {
        return None;
    }
    let scope_query = match scope {
        None => String::new(),
        Some(EVERY_PROJECT) => String::from("&all-projects=true"),
        Some(project) if is_name(project) => format!("&project={project}"),
        Some(_) => return None,
    };
    Some(Collection { key: key.to_owned(), path: format!("/v1/{plural}?watch=true{scope_query}") })
}

#[derive(Deserialize)]
pub struct Subscription {
    collections: String,
}

enum Merged {
    Event(Event),
    Ended,
}

fn quoted(text: &str) -> String {
    serde_json::Value::String(text.to_owned()).to_string()
}

fn lines(body: ResponseBody) -> impl futures::Stream<Item = String> {
    futures::stream::unfold((body, BytesMut::new()), |(mut body, mut buffer)| async move {
        loop {
            if let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
                let line = buffer.split_to(position + 1);
                return Some((String::from_utf8_lossy(&line[..position]).into_owned(), (body, buffer)));
            }
            match body.next().await? {
                Ok(chunk) => buffer.extend_from_slice(&chunk),
                Err(_) => return None,
            }
        }
    })
}

async fn watch(state: DashboardState, token: String, collection: Collection) -> BoxStream<'static, Merged> {
    let ended = || futures::stream::once(async { Merged::Ended }).boxed();
    let Some(request) = upstream_request(Method::GET, &collection.path, &token, Bytes::new()) else {
        return ended();
    };
    let key = quoted(&collection.key);
    match state.open(request).await {
        Ok(answer) if answer.status() == StatusCode::OK => lines(answer.into_body())
            .map(move |line| {
                Merged::Event(Event::default().data(format!("{{\"collection\":{key},\"frame\":{line}}}")))
            })
            .chain(futures::stream::once(async { Merged::Ended }))
            .boxed(),
        Ok(answer) => {
            let refused = format!("{{\"collection\":{key},\"refused\":{}}}", answer.status().as_u16());
            futures::stream::once(async move { Merged::Event(Event::default().data(refused)) })
                .chain(futures::stream::pending())
                .boxed()
        }
        Err(_) => ended(),
    }
}

pub async fn changes(
    State(state): State<DashboardState>,
    Query(subscription): Query<Subscription>,
    headers: HeaderMap,
) -> Response {
    let Some(session) = current_session(&state, &headers) else {
        return refusal(StatusCode::UNAUTHORIZED, "sign in first");
    };
    let requested: Option<BTreeSet<Collection>> =
        subscription.collections.split(',').filter(|key| !key.is_empty()).map(collection).collect();
    let Some(collections) =
        requested.filter(|collections| !collections.is_empty() && collections.len() <= LARGEST_SUBSCRIPTION)
    else {
        return refusal(StatusCode::BAD_REQUEST, "name between one and 32 collections such as vms@project");
    };
    let watches = futures::future::join_all(
        collections.into_iter().map(|collection| watch(state.clone(), session.token.clone(), collection)),
    )
    .await;
    let merged = futures::stream::select_all(watches)
        .take_while(|item| futures::future::ready(matches!(item, Merged::Event(_))))
        .filter_map(|item| {
            futures::future::ready(match item {
                Merged::Event(event) => Some(Ok::<_, Infallible>(event)),
                Merged::Ended => None,
            })
        });
    Sse::new(merged).keep_alive(KeepAlive::default()).into_response()
}

#[cfg(test)]
mod tests {
    use super::collection;

    #[test]
    fn collections_name_a_kind_and_where_to_look() {
        assert_eq!(collection("nodes").map(|found| found.path), Some(String::from("/v1/nodes?watch=true")));
        assert_eq!(
            collection("vms@default").map(|found| found.path),
            Some(String::from("/v1/vms?watch=true&project=default"))
        );
        assert_eq!(
            collection("vms@*").map(|found| found.path),
            Some(String::from("/v1/vms?watch=true&all-projects=true"))
        );
        for refused in ["", "vms@", "../nodes", "vms@default&all-projects=true", "Vms"] {
            assert_eq!(collection(refused), None, "{refused}");
        }
    }
}
