use ring::hmac;

use crate::election::Advertisement;

pub const ADVERTISEMENT_PORT: u16 = 7447;
const TAG_BYTES: usize = 32;
const KEY_PURPOSE: &[u8] = b"amafu cluster address advertisements";

#[derive(Debug)]
pub struct AdvertisementKey(hmac::Key);

impl AdvertisementKey {
    #[must_use]
    pub fn derived_from(session_key: &[u8]) -> Self {
        let parent = hmac::Key::new(hmac::HMAC_SHA256, session_key);
        let derived = hmac::sign(&parent, KEY_PURPOSE);
        Self(hmac::Key::new(hmac::HMAC_SHA256, derived.as_ref()))
    }

    #[must_use]
    pub fn seal(&self, advertisement: &Advertisement) -> Vec<u8> {
        let mut message = serde_json::to_vec(advertisement).unwrap_or_default();
        let tag = hmac::sign(&self.0, &message);
        message.extend_from_slice(tag.as_ref());
        message
    }

    #[must_use]
    pub fn open(&self, message: &[u8]) -> Option<Advertisement> {
        let body_length = message.len().checked_sub(TAG_BYTES)?;
        let (body, tag) = message.split_at(body_length);
        hmac::verify(&self.0, body, tag).ok()?;
        serde_json::from_slice(body).ok()
    }
}

#[cfg(test)]
mod tests {
    use amafu_domain::NodeName;

    use super::*;

    fn advertisement() -> Advertisement {
        Advertisement { node: NodeName::new("node-1").unwrap(), is_eligible: true, holds: true }
    }

    #[test]
    fn an_advertisement_opens_only_under_the_key_it_was_sealed_with() {
        let key = AdvertisementKey::derived_from(&[7; 32]);
        let sealed = key.seal(&advertisement());
        assert_eq!(key.open(&sealed), Some(advertisement()));
        assert_eq!(AdvertisementKey::derived_from(&[8; 32]).open(&sealed), None);
    }

    #[test]
    fn a_tampered_or_truncated_advertisement_is_ignored() {
        let key = AdvertisementKey::derived_from(&[7; 32]);
        let mut sealed = key.seal(&advertisement());
        let holds = sealed.iter().position(|byte| *byte == b't').unwrap();
        sealed[holds] = b'f';
        assert_eq!(key.open(&sealed), None);
        assert_eq!(key.open(&[0; 8]), None);
    }
}
