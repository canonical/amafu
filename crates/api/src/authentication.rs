use std::path::Path;

use http::HeaderMap;
use http::header::AUTHORIZATION;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::identity::Principal;

const BEARER_PREFIX: &str = "Bearer ";
const TOKEN_PREFIX: &str = "amafu-";

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AuthenticationError {
    #[error("the request carries no credentials")]
    MissingCredentials,
    #[error("the credentials are malformed")]
    MalformedCredentials,
    #[error("the credentials are not recognized")]
    UnknownCredentials,
    #[error("the credentials have expired")]
    ExpiredCredentials,
}

pub trait Authenticator: Send + Sync {
    fn authenticate(
        &self,
        headers: &HeaderMap,
        now: OffsetDateTime,
    ) -> Result<Principal, AuthenticationError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenIdentity {
    #[serde(flatten)]
    pub principal: Principal,
    #[serde(with = "hex::serde")]
    pub token_sha256: [u8; 32],
    #[serde(default, with = "time::serde::rfc3339::option", skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<OffsetDateTime>,
}

impl TokenIdentity {
    #[must_use]
    pub fn for_token(principal: Principal, token: &str, expires_at: Option<OffsetDateTime>) -> Self {
        Self { principal, token_sha256: digest(token), expires_at }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenFile {
    pub identities: Vec<TokenIdentity>,
}

#[derive(Debug, thiserror::Error)]
pub enum TokenFileError {
    #[error("cannot read the token file: {0}")]
    Read(#[from] std::io::Error),
    #[error("the token file is malformed: {0}")]
    Parse(#[from] serde_yaml_ng::Error),
}

#[derive(Debug, Clone, Default)]
pub struct TokenAuthenticator {
    identities: Vec<TokenIdentity>,
}

#[must_use]
pub fn generate_token() -> String {
    format!("{TOKEN_PREFIX}{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

impl TokenAuthenticator {
    #[must_use]
    pub fn new(identities: Vec<TokenIdentity>) -> Self {
        Self { identities }
    }

    pub fn from_file(path: &Path) -> Result<Self, TokenFileError> {
        let contents = std::fs::read_to_string(path)?;
        let file: TokenFile = serde_yaml_ng::from_str(&contents)?;
        Ok(Self::new(file.identities))
    }

    fn find(&self, token: &str) -> Option<&TokenIdentity> {
        let presented = digest(token);
        let mut found = None;
        for identity in &self.identities {
            if bool::from(identity.token_sha256.ct_eq(&presented)) {
                found = Some(identity);
            }
        }
        found
    }
}

impl Authenticator for TokenAuthenticator {
    fn authenticate(
        &self,
        headers: &HeaderMap,
        now: OffsetDateTime,
    ) -> Result<Principal, AuthenticationError> {
        let header = headers.get(AUTHORIZATION).ok_or(AuthenticationError::MissingCredentials)?;
        let header = header.to_str().map_err(|_| AuthenticationError::MalformedCredentials)?;
        let token = header.strip_prefix(BEARER_PREFIX).ok_or(AuthenticationError::MalformedCredentials)?;
        if !token.starts_with(TOKEN_PREFIX) {
            return Err(AuthenticationError::MalformedCredentials);
        }
        let identity = self.find(token).ok_or(AuthenticationError::UnknownCredentials)?;
        if identity.expires_at.is_some_and(|expires_at| expires_at <= now) {
            return Err(AuthenticationError::ExpiredCredentials);
        }
        Ok(identity.principal.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use amafu_domain::ProjectName;
    use amafu_domain::access::Role;
    use http::HeaderValue;

    use super::*;

    fn principal() -> Principal {
        Principal {
            name: String::from("alice"),
            role: Role::Member,
            project: ProjectName::new("project-a").unwrap(),
        }
    }

    fn headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(value).unwrap());
        headers
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::from_secs(1_000)
    }

    #[test]
    fn a_known_token_authenticates_its_principal() {
        let token = generate_token();
        let authenticator =
            TokenAuthenticator::new(vec![TokenIdentity::for_token(principal(), &token, None)]);
        assert_eq!(authenticator.authenticate(&headers(&format!("Bearer {token}")), now()), Ok(principal()));
    }

    #[test]
    fn missing_malformed_and_unknown_tokens_are_refused() {
        let authenticator =
            TokenAuthenticator::new(vec![TokenIdentity::for_token(principal(), &generate_token(), None)]);
        assert_eq!(
            authenticator.authenticate(&HeaderMap::new(), now()),
            Err(AuthenticationError::MissingCredentials)
        );
        assert_eq!(
            authenticator.authenticate(&headers("Basic abc"), now()),
            Err(AuthenticationError::MalformedCredentials)
        );
        assert_eq!(
            authenticator.authenticate(&headers("Bearer abc"), now()),
            Err(AuthenticationError::MalformedCredentials)
        );
        assert_eq!(
            authenticator.authenticate(&headers(&format!("Bearer {}", generate_token())), now()),
            Err(AuthenticationError::UnknownCredentials)
        );
    }

    #[test]
    fn an_expired_token_is_refused() {
        let token = generate_token();
        let identity = TokenIdentity::for_token(principal(), &token, Some(now()));
        let authenticator = TokenAuthenticator::new(vec![identity]);
        assert_eq!(
            authenticator.authenticate(&headers(&format!("Bearer {token}")), now()),
            Err(AuthenticationError::ExpiredCredentials)
        );
    }

    #[test]
    fn the_token_file_stores_only_digests() {
        let token = generate_token();
        let file = TokenFile { identities: vec![TokenIdentity::for_token(principal(), &token, None)] };
        let encoded = serde_yaml_ng::to_string(&file).unwrap();
        assert!(!encoded.contains(&token));
        let decoded: TokenFile = serde_yaml_ng::from_str(&encoded).unwrap();
        assert_eq!(decoded, file);
    }
}
