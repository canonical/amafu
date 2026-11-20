use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::signature::{
    ECDSA_P256_SHA256_FIXED, RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents, UnparsedPublicKey,
};
use serde_json::Value;
use time::OffsetDateTime;

use crate::keys::IssuerError;

const UNCOMPRESSED_POINT: u8 = 0x04;

#[derive(Debug, Clone, PartialEq, Eq)]
enum KeyMaterial {
    Rsa { modulus: Vec<u8>, exponent: Vec<u8> },
    P256 { point: Vec<u8> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExternalKey {
    key_id: Option<String>,
    material: KeyMaterial,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExternalKeys {
    keys: Vec<ExternalKey>,
}

fn decoded(value: &Value) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(value.as_str()?).ok()
}

fn parse_key(key: &Value) -> Option<ExternalKey> {
    let material = match (key["kty"].as_str()?, key["crv"].as_str()) {
        ("RSA", _) => KeyMaterial::Rsa { modulus: decoded(&key["n"])?, exponent: decoded(&key["e"])? },
        ("EC", Some("P-256")) => {
            let mut point = vec![UNCOMPRESSED_POINT];
            point.extend(decoded(&key["x"])?);
            point.extend(decoded(&key["y"])?);
            KeyMaterial::P256 { point }
        }
        _ => return None,
    };
    Some(ExternalKey { key_id: key["kid"].as_str().map(str::to_owned), material })
}

impl ExternalKeys {
    #[must_use]
    pub fn parse(document: &Value) -> Self {
        let keys = document["keys"]
            .as_array()
            .map(|keys| keys.iter().filter_map(parse_key).collect())
            .unwrap_or_default();
        Self { keys }
    }

    fn candidates<'a>(&'a self, key_id: Option<&'a str>) -> impl Iterator<Item = &'a ExternalKey> {
        self.keys.iter().filter(move |key| key_id.is_none() || key.key_id.as_deref() == key_id)
    }
}

fn part(token: &str, index: usize) -> Result<Value, IssuerError> {
    let segment = token.split('.').nth(index).ok_or(IssuerError::Malformed)?;
    let bytes = URL_SAFE_NO_PAD.decode(segment).map_err(|_| IssuerError::Malformed)?;
    serde_json::from_slice(&bytes).map_err(|_| IssuerError::Malformed)
}

pub fn unverified_claims(token: &str) -> Result<Value, IssuerError> {
    part(token, 1)
}

pub fn verify_external(token: &str, keys: &ExternalKeys, now: OffsetDateTime) -> Result<Value, IssuerError> {
    let header = part(token, 0)?;
    let (signing_input, signature) = token.rsplit_once('.').ok_or(IssuerError::Malformed)?;
    let signature = URL_SAFE_NO_PAD.decode(signature).map_err(|_| IssuerError::Malformed)?;
    let algorithm = header["alg"].as_str().ok_or(IssuerError::Malformed)?;
    let verified = keys.candidates(header["kid"].as_str()).any(|key| match (&key.material, algorithm) {
        (KeyMaterial::Rsa { modulus, exponent }, "RS256") => {
            RsaPublicKeyComponents { n: modulus, e: exponent }
                .verify(&RSA_PKCS1_2048_8192_SHA256, signing_input.as_bytes(), &signature)
                .is_ok()
        }
        (KeyMaterial::P256 { point }, "ES256") => UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, point)
            .verify(signing_input.as_bytes(), &signature)
            .is_ok(),
        _ => false,
    });
    if !verified {
        return Err(IssuerError::BadSignature);
    }
    let claims = unverified_claims(token)?;
    let now = now.unix_timestamp();
    let expires = claims["exp"].as_i64().ok_or(IssuerError::Malformed)?;
    let not_before = claims["nbf"].as_i64().unwrap_or(i64::MIN);
    if expires <= now || not_before > now {
        return Err(IssuerError::Expired);
    }
    Ok(claims)
}

#[must_use]
pub fn audiences(claims: &Value) -> Vec<String> {
    match &claims["aud"] {
        Value::String(audience) => vec![audience.clone()],
        Value::Array(audiences) => {
            audiences.iter().filter_map(|audience| audience.as_str().map(str::to_owned)).collect()
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use super::*;
    use crate::claims::Claims;
    use crate::documents::JwkSet;
    use crate::keys::KeyPair;

    fn now() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::days(20_000)
    }

    fn published(key: &KeyPair) -> ExternalKeys {
        ExternalKeys::parse(&serde_json::to_value(JwkSet::of([&key.public_key()])).unwrap())
    }

    #[test]
    fn a_token_from_a_trusted_issuer_verifies_against_its_published_keys() {
        let key = KeyPair::generate().unwrap();
        let token =
            Claims::new("https://ci.example", "repo:web:ref:main", "amafu", now(), Duration::minutes(5))
                .sign(&key)
                .unwrap()
                .token;
        let claims = verify_external(&token, &published(&key), now()).unwrap();
        assert_eq!(claims["sub"], "repo:web:ref:main");
        assert_eq!(audiences(&claims), ["amafu"]);
        assert_eq!(unverified_claims(&token).unwrap()["iss"], "https://ci.example");
    }

    #[test]
    fn other_keys_and_expired_tokens_are_refused() {
        let key = KeyPair::generate().unwrap();
        let token = Claims::new("https://ci.example", "x", "amafu", now(), Duration::minutes(5))
            .sign(&key)
            .unwrap()
            .token;
        let stranger = KeyPair::generate().unwrap();
        assert!(matches!(
            verify_external(&token, &published(&stranger), now()),
            Err(IssuerError::BadSignature)
        ));
        let later = now() + Duration::minutes(6);
        assert!(matches!(verify_external(&token, &published(&key), later), Err(IssuerError::Expired)));
    }

    #[test]
    fn audiences_may_be_a_list() {
        assert_eq!(audiences(&serde_json::json!({"aud": ["a", "b"]})), ["a", "b"]);
        assert_eq!(audiences(&serde_json::json!({})).len(), 0);
    }
}
