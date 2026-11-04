use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

pub const SESSION_KEY_BYTES: usize = 32;
const ASSOCIATED_DATA: &[u8] = b"amafu-dashboard-session/1";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    #[error("the session key must be {SESSION_KEY_BYTES} bytes in hexadecimal")]
    MalformedKey,
    #[error("the session cannot be sealed")]
    Sealing,
    #[error("the session is not one this dashboard issued")]
    Forged,
    #[error("the session has expired")]
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub token: String,
    pub project: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

pub struct SessionKey {
    key: LessSafeKey,
    random: SystemRandom,
}

impl SessionKey {
    pub fn from_hexadecimal(text: &str) -> Result<Self, SessionError> {
        let bytes = hex::decode(text.trim()).map_err(|_| SessionError::MalformedKey)?;
        if bytes.len() != SESSION_KEY_BYTES {
            return Err(SessionError::MalformedKey);
        }
        let key = UnboundKey::new(&AES_256_GCM, &bytes).map_err(|_| SessionError::MalformedKey)?;
        Ok(Self { key: LessSafeKey::new(key), random: SystemRandom::new() })
    }

    pub fn generate_hexadecimal() -> Result<String, SessionError> {
        let mut bytes = [0_u8; SESSION_KEY_BYTES];
        SystemRandom::new().fill(&mut bytes).map_err(|_| SessionError::Sealing)?;
        Ok(hex::encode(bytes))
    }

    pub fn seal(&self, session: &Session) -> Result<String, SessionError> {
        let mut nonce = [0_u8; NONCE_LEN];
        self.random.fill(&mut nonce).map_err(|_| SessionError::Sealing)?;
        let mut sealed = serde_json::to_vec(session).map_err(|_| SessionError::Sealing)?;
        self.key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(ASSOCIATED_DATA),
                &mut sealed,
            )
            .map_err(|_| SessionError::Sealing)?;
        Ok(URL_SAFE_NO_PAD.encode([nonce.as_slice(), &sealed].concat()))
    }

    pub fn open(&self, cookie: &str, now: OffsetDateTime) -> Result<Session, SessionError> {
        let bytes = URL_SAFE_NO_PAD.decode(cookie).map_err(|_| SessionError::Forged)?;
        if bytes.len() <= NONCE_LEN {
            return Err(SessionError::Forged);
        }
        let (nonce, sealed) = bytes.split_at(NONCE_LEN);
        let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| SessionError::Forged)?;
        let mut sealed = sealed.to_vec();
        let opened = self
            .key
            .open_in_place(nonce, Aad::from(ASSOCIATED_DATA), &mut sealed)
            .map_err(|_| SessionError::Forged)?;
        let session: Session = serde_json::from_slice(opened).map_err(|_| SessionError::Forged)?;
        if session.expires_at <= now {
            return Err(SessionError::Expired);
        }
        Ok(session)
    }
}

#[must_use]
pub fn session_lasting(
    token: String,
    project: Option<String>,
    now: OffsetDateTime,
    lifetime: Duration,
) -> Session {
    Session { token, project, expires_at: now + lifetime }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SessionKey {
        SessionKey::from_hexadecimal(&SessionKey::generate_hexadecimal().unwrap()).unwrap()
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    #[test]
    fn a_sealed_session_opens_only_with_the_same_key_and_before_it_expires() {
        let key = key();
        let session = session_lasting(
            String::from("amafu-token"),
            Some(String::from("project-a")),
            now(),
            Duration::from_secs(60),
        );
        let cookie = key.seal(&session).unwrap();
        assert!(!cookie.contains("amafu-token"));
        assert_eq!(key.open(&cookie, now()), Ok(session));
        assert_eq!(key.open(&cookie, now() + Duration::from_secs(61)), Err(SessionError::Expired));
        assert_eq!(self::key().open(&cookie, now()), Err(SessionError::Forged));
    }

    #[test]
    fn a_tampered_session_is_refused() {
        let key = key();
        let cookie =
            key.seal(&session_lasting(String::from("token"), None, now(), Duration::from_secs(60))).unwrap();
        let mut bytes = URL_SAFE_NO_PAD.decode(&cookie).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        assert_eq!(key.open(&URL_SAFE_NO_PAD.encode(bytes), now()), Err(SessionError::Forged));
        assert_eq!(key.open("not-a-session", now()), Err(SessionError::Forged));
        assert!(SessionKey::from_hexadecimal("abcd").is_err());
    }
}
