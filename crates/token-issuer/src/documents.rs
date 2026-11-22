use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::keys::PublicKey;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Jwk {
    pub kty: String,
    pub crv: String,
    pub alg: String,
    #[serde(rename = "use")]
    pub usage: String,
    pub kid: String,
    #[serde(rename = "x")]
    pub x_coordinate: String,
    #[serde(rename = "y")]
    pub y_coordinate: String,
}

impl Jwk {
    #[must_use]
    pub fn of(key: &PublicKey) -> Option<Self> {
        let (x_coordinate, y_coordinate) = key.coordinates()?;
        Some(Self {
            kty: String::from("EC"),
            crv: String::from("P-256"),
            alg: String::from("ES256"),
            usage: String::from("sig"),
            kid: key.key_id(),
            x_coordinate: URL_SAFE_NO_PAD.encode(x_coordinate),
            y_coordinate: URL_SAFE_NO_PAD.encode(y_coordinate),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct JwkSet {
    pub keys: Vec<Jwk>,
}

impl JwkSet {
    #[must_use]
    pub fn of<'a>(keys: impl IntoIterator<Item = &'a PublicKey>) -> Self {
        Self { keys: keys.into_iter().filter_map(Jwk::of).collect() }
    }
}

#[must_use]
pub fn discovery_document(issuer: &str, jwks_uri: &str) -> Value {
    serde_json::json!({
        "issuer": issuer,
        "jwks_uri": jwks_uri,
        "id_token_signing_alg_values_supported": ["ES256"],
        "response_types_supported": ["id_token"],
        "subject_types_supported": ["public"],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::KeyPair;

    #[test]
    fn a_key_set_publishes_each_key_as_a_p256_jwk() {
        let (first, second) = (KeyPair::generate().unwrap(), KeyPair::generate().unwrap());
        let set = JwkSet::of([&first.public_key(), &second.public_key()]);
        assert_eq!(set.keys.len(), 2);
        assert_eq!(set.keys[0].kid, first.public_key().key_id());
        assert_eq!(URL_SAFE_NO_PAD.decode(&set.keys[0].x_coordinate).unwrap().len(), 32);
        assert!(serde_json::to_value(&set.keys[0]).unwrap()["x"].is_string());
        assert_eq!(serde_json::to_value(&set.keys[0]).unwrap()["use"], "sig");
    }

    #[test]
    fn discovery_points_at_the_key_set() {
        let document = discovery_document("https://amafu.example", "https://amafu.example/.well-known/jwks.json");
        assert_eq!(document["jwks_uri"], "https://amafu.example/.well-known/jwks.json");
        assert_eq!(document["id_token_signing_alg_values_supported"][0], "ES256");
    }
}
