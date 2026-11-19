use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::{Duration, OffsetDateTime};

use crate::keys::{IssuerError, KeyPair, PublicKey};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Claims {
    pub iss: String,
    pub sub: String,
    pub aud: String,
    pub iat: i64,
    pub nbf: i64,
    pub exp: i64,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

impl Claims {
    #[must_use]
    pub fn new(issuer: &str, subject: &str, audience: &str, now: OffsetDateTime, lifetime: Duration) -> Self {
        let issued_at = now.unix_timestamp();
        Self {
            iss: issuer.to_owned(),
            sub: subject.to_owned(),
            aud: audience.to_owned(),
            iat: issued_at,
            nbf: issued_at,
            exp: (now + lifetime).unix_timestamp(),
            extra: serde_json::Map::new(),
        }
    }

    #[must_use]
    pub fn with(mut self, claim: &str, value: Value) -> Self {
        self.extra.insert(claim.to_owned(), value);
        self
    }

    pub fn sign(&self, key: &KeyPair) -> Result<Issued, IssuerError> {
        let header = serde_json::json!({"alg": "ES256", "typ": "JWT", "kid": key.public_key().key_id()});
        let signing_input = format!("{}.{}", encoded(&header)?, encoded(self)?);
        let signature = URL_SAFE_NO_PAD.encode(key.sign(signing_input.as_bytes())?);
        Ok(Issued { token: format!("{signing_input}.{signature}"), expires_at: self.exp })
    }

    pub fn verified(token: &str, key: &PublicKey) -> Result<Self, IssuerError> {
        let (signing_input, signature) = token.rsplit_once('.').ok_or(IssuerError::Malformed)?;
        let (_, payload) = signing_input.split_once('.').ok_or(IssuerError::Malformed)?;
        let signature = URL_SAFE_NO_PAD.decode(signature).map_err(|_| IssuerError::Malformed)?;
        key.verify(signing_input.as_bytes(), &signature)?;
        let payload = URL_SAFE_NO_PAD.decode(payload).map_err(|_| IssuerError::Malformed)?;
        serde_json::from_slice(&payload).map_err(|_| IssuerError::Malformed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issued {
    pub token: String,
    pub expires_at: i64,
}

fn encoded(value: &impl Serialize) -> Result<String, IssuerError> {
    let json = serde_json::to_vec(value).map_err(|error| IssuerError::Encoding(error.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(json))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims() -> Claims {
        Claims::new(
            "https://amafu.example",
            "alice@amafu",
            "cluster-uid",
            OffsetDateTime::UNIX_EPOCH + Duration::days(20_000),
            Duration::minutes(5),
        )
        .with("groups", serde_json::json!(["amafu:web-team:member"]))
    }

    #[test]
    fn a_signed_token_verifies_with_its_public_key_and_keeps_its_claims() {
        let key = KeyPair::generate().unwrap();
        let issued = claims().sign(&key).unwrap();
        assert_eq!(issued.expires_at - claims().iat, 300);
        assert_eq!(Claims::verified(&issued.token, &key.public_key()).unwrap(), claims());
    }

    #[test]
    fn another_key_or_a_changed_payload_does_not_verify() {
        let key = KeyPair::generate().unwrap();
        let issued = claims().sign(&key).unwrap();
        assert!(Claims::verified(&issued.token, &KeyPair::generate().unwrap().public_key()).is_err());
        let mut parts: Vec<&str> = issued.token.split('.').collect();
        let forged = URL_SAFE_NO_PAD.encode(br#"{"sub":"mallory"}"#);
        parts[1] = &forged;
        assert!(Claims::verified(&parts.join("."), &key.public_key()).is_err());
    }

    #[test]
    fn the_header_names_the_algorithm_and_key() {
        let key = KeyPair::generate().unwrap();
        let token = claims().sign(&key).unwrap().token;
        let header: Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(token.split('.').next().unwrap()).unwrap())
                .unwrap();
        assert_eq!(header["alg"], "ES256");
        assert_eq!(header["kid"], key.public_key().key_id());
    }

    #[test]
    fn a_key_read_back_from_its_pkcs8_signs_the_same_way() {
        let key = KeyPair::generate().unwrap();
        let restored = KeyPair::from_pkcs8(key.pkcs8()).unwrap();
        assert_eq!(restored.public_key(), key.public_key());
    }
}
