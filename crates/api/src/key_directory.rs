use std::time::Duration;

use amafu_domain::access::Verb;
use amafu_domain::kinds::ssh_key::{KeyLookup, KeySource, SshKey, is_account_name};
use amafu_domain::registry::KindDescriptor;
use http::{StatusCode, Uri};

use crate::handlers::{ApiError, ApiSuccess, Operation};
use crate::https::https_client;
use crate::request_chain::RequestContext;
use crate::routes::ApiState;
use crate::wire::ErrorCode;

const DIRECTORY_TIMEOUT: Duration = Duration::from_secs(10);
const LONGEST_LISTING: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DirectoryError {
    #[error("{0} has no such account")]
    UnknownAccount(String),
    #[error("{0}")]
    Unreachable(String),
}

#[async_trait::async_trait]
pub trait KeyDirectory: Send + Sync {
    async fn listing(&self, source: KeySource, account: &str) -> Result<String, DirectoryError>;
}

pub struct HttpKeyDirectory {
    client: Option<reqwest::Client>,
}

impl Default for HttpKeyDirectory {
    fn default() -> Self {
        Self { client: https_client(DIRECTORY_TIMEOUT) }
    }
}

#[async_trait::async_trait]
impl KeyDirectory for HttpKeyDirectory {
    async fn listing(&self, source: KeySource, account: &str) -> Result<String, DirectoryError> {
        let unreachable =
            |error: reqwest::Error| DirectoryError::Unreachable(error.without_url().to_string());
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| DirectoryError::Unreachable(String::from("no HTTPS client")))?;
        let url = source.keys_url(account);
        let response = client.get(&url).send().await.map_err(unreachable)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(DirectoryError::UnknownAccount(url));
        }
        if !response.status().is_success() {
            return Err(DirectoryError::Unreachable(format!("{url} answered {}", response.status())));
        }
        let body = response.bytes().await.map_err(unreachable)?;
        if body.len() > LONGEST_LISTING {
            return Err(DirectoryError::Unreachable(format!("{url} lists more keys than anyone needs")));
        }
        Ok(String::from_utf8_lossy(&body).into_owned())
    }
}

pub async fn look_up(
    state: ApiState,
    context: RequestContext,
    target: Uri,
    source: String,
    account: String,
) -> Result<ApiSuccess, ApiError> {
    let operation = Operation::new(KindDescriptor::of::<SshKey>(), Verb::Create, context, &target, None)?;
    operation.authorize()?;
    let invalid = |message: String| operation.error(StatusCode::BAD_REQUEST, ErrorCode::Malformed, message);
    let source: KeySource = source.parse().map_err(invalid)?;
    if !is_account_name(&account) {
        return Err(invalid(format!("{account:?} is not an account name")));
    }
    let listing = state.key_directory.listing(source, &account).await.map_err(|error| match error {
        DirectoryError::UnknownAccount(_) => {
            operation.error(StatusCode::NOT_FOUND, ErrorCode::NotFound, error.to_string())
        }
        DirectoryError::Unreachable(_) => {
            operation.error(StatusCode::BAD_GATEWAY, ErrorCode::Unavailable, error.to_string())
        }
    })?;
    operation.json(StatusCode::OK, &KeyLookup::from_listing(source, &account, &listing))
}
