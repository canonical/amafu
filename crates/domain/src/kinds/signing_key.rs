use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

use crate::access::ClusterPolicy;
use crate::resource::{Resource, Scope};
use crate::validation::{Validate, ValidationError};

pub const KEY_LIFETIME: Duration = Duration::days(30);
pub const PUBLISHED_AHEAD: Duration = Duration::days(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SigningKey;

impl Resource for SigningKey {
    const KIND: &'static str = "SigningKey";
    const PLURAL_NAME: &'static str = "signing-keys";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Cluster;
    const CLUSTER_POLICY: ClusterPolicy = ClusterPolicy::AdminOnly;
    type Specification = SigningKeySpecification;
    type Status = SigningKeyStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SigningKeySpecification {
    pub private_key_pkcs8: String,
    #[serde(with = "time::serde::rfc3339")]
    pub signs_from: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub signs_until: OffsetDateTime,
}

impl SigningKeySpecification {
    #[must_use]
    pub fn window_starting(private_key_pkcs8: String, signs_from: OffsetDateTime) -> Self {
        Self { private_key_pkcs8, signs_from, signs_until: signs_from + KEY_LIFETIME }
    }

    #[must_use]
    pub fn signs_at(&self, now: OffsetDateTime) -> bool {
        self.signs_from <= now && now < self.signs_until
    }

    #[must_use]
    pub fn is_published_at(&self, now: OffsetDateTime) -> bool {
        self.signs_from - PUBLISHED_AHEAD <= now && now < self.signs_until + PUBLISHED_AHEAD
    }
}

impl Validate for SigningKeySpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(
            self.signs_from < self.signs_until,
            "specification.signs_until",
            "must follow signs_from",
        );
        error.check(
            !self.private_key_pkcs8.is_empty(),
            "specification.private_key_pkcs8",
            "must not be empty",
        );
        error.into_result()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SigningKeyStatus {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_published_a_day_before_and_after_it_signs() {
        let start = OffsetDateTime::UNIX_EPOCH + Duration::days(100);
        let key = SigningKeySpecification::window_starting(String::from("key"), start);
        assert!(!key.signs_at(start - Duration::hours(1)));
        assert!(key.is_published_at(start - Duration::hours(1)));
        assert!(key.signs_at(start + Duration::days(29)));
        assert!(!key.signs_at(start + KEY_LIFETIME));
        assert!(key.is_published_at(start + KEY_LIFETIME + Duration::hours(23)));
        assert!(!key.is_published_at(start + KEY_LIFETIME + Duration::days(2)));
    }
}
