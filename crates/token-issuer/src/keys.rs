use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::rand::SystemRandom;
use ring::signature::{
    ECDSA_P256_SHA256_FIXED, ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair as _, UnparsedPublicKey,
};
use sha2::{Digest, Sha256};

const UNCOMPRESSED_POINT: u8 = 0x04;
const COORDINATE_BYTES: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum IssuerError {
    #[error("the signing key cannot be read")]
    UnreadableKey,
    #[error("signing failed")]
    Signing,
    #[error("the claims cannot be encoded: {0}")]
    Encoding(String),
    #[error("the token is malformed")]
    Malformed,
    #[error("the token signature does not verify")]
    BadSignature,
}

pub struct KeyPair {
    pkcs8: Vec<u8>,
    pair: EcdsaKeyPair,
}

impl KeyPair {
    pub fn generate() -> Result<Self, IssuerError> {
        let random = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &random)
            .map_err(|_| IssuerError::UnreadableKey)?;
        Self::from_pkcs8(pkcs8.as_ref())
    }

    pub fn from_pkcs8(pkcs8: &[u8]) -> Result<Self, IssuerError> {
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8, &SystemRandom::new())
            .map_err(|_| IssuerError::UnreadableKey)?;
        Ok(Self { pkcs8: pkcs8.to_vec(), pair })
    }

    #[must_use]
    pub fn pkcs8(&self) -> &[u8] {
        &self.pkcs8
    }

    #[must_use]
    pub fn public_key(&self) -> PublicKey {
        PublicKey { point: self.pair.public_key().as_ref().to_vec() }
    }

    pub(crate) fn sign(&self, message: &[u8]) -> Result<Vec<u8>, IssuerError> {
        let signature = self.pair.sign(&SystemRandom::new(), message).map_err(|_| IssuerError::Signing)?;
        Ok(signature.as_ref().to_vec())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKey {
    point: Vec<u8>,
}

impl PublicKey {
    #[must_use]
    pub fn key_id(&self) -> String {
        URL_SAFE_NO_PAD.encode(&Sha256::digest(&self.point)[..16])
    }

    pub(crate) fn coordinates(&self) -> Option<(&[u8], &[u8])> {
        let (&tag, rest) = self.point.split_first()?;
        (tag == UNCOMPRESSED_POINT && rest.len() == 2 * COORDINATE_BYTES)
            .then(|| rest.split_at(COORDINATE_BYTES))
    }

    pub fn verify(&self, message: &[u8], signature: &[u8]) -> Result<(), IssuerError> {
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, &self.point)
            .verify(message, signature)
            .map_err(|_| IssuerError::BadSignature)
    }
}
