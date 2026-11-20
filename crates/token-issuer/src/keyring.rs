use std::sync::Arc;

use amafu_domain::kinds::signing_key::{SigningKey, SigningKeySpecification};
use amafu_interfaces::objects::list;
use amafu_interfaces::{Consistency, Store};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use time::OffsetDateTime;

use crate::claims::{Claims, Issued};
use crate::documents::JwkSet;
use crate::keys::{IssuerError, KeyPair, PublicKey};

#[derive(Debug, thiserror::Error)]
pub enum KeyringError {
    #[error("the signing keys cannot be read: {0}")]
    Unreadable(String),
    #[error("no signing key is active yet")]
    NoActiveKey,
    #[error(transparent)]
    Issuer(#[from] IssuerError),
}

#[derive(Clone)]
pub struct Keyring {
    store: Arc<dyn Store>,
}

impl Keyring {
    #[must_use]
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }

    pub async fn published(&self, now: OffsetDateTime) -> Result<JwkSet, KeyringError> {
        Ok(JwkSet::of(&self.public_keys(now).await?))
    }

    pub async fn public_keys(&self, now: OffsetDateTime) -> Result<Vec<PublicKey>, KeyringError> {
        Ok(self.published_keys(now).await?.iter().map(|(_, key)| key.public_key()).collect())
    }

    pub async fn sign(&self, claims: &Claims, now: OffsetDateTime) -> Result<Issued, KeyringError> {
        let key = self
            .published_keys(now)
            .await?
            .into_iter()
            .find(|(specification, _)| specification.signs_at(now))
            .map(|(_, key)| key)
            .ok_or(KeyringError::NoActiveKey)?;
        Ok(claims.sign(&key)?)
    }

    async fn published_keys(
        &self,
        now: OffsetDateTime,
    ) -> Result<Vec<(SigningKeySpecification, KeyPair)>, KeyringError> {
        let keys = list::<SigningKey>(self.store.as_ref(), None, Consistency::Linearizable)
            .await
            .map_err(|error| KeyringError::Unreadable(error.to_string()))?;
        Ok(keys
            .into_iter()
            .map(|key| key.specification)
            .filter(|specification| specification.is_published_at(now))
            .filter_map(|specification| {
                let pkcs8 = STANDARD.decode(&specification.private_key_pkcs8).ok()?;
                KeyPair::from_pkcs8(&pkcs8).ok().map(|key| (specification, key))
            })
            .collect())
    }
}
