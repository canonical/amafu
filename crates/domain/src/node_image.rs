use std::fmt;

use serde::{Deserialize, Serialize};

use crate::digest::Sha256Digest;
use crate::features::FeatureLevel;
use crate::validation::ValidationError;

const LONGEST_VERSION: usize = 64;
const LONGEST_URL: usize = 2048;
const SIGNATURE_HEXADECIMAL_LENGTH: usize = 128;
const SIGNED_PAYLOAD_HEADER: &str = "amafu-node-image/1";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageIdentity {
    pub version: String,
    pub digest: Sha256Digest,
}

impl fmt::Display for ImageIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let digest = self.digest.hexadecimal();
        write!(formatter, "{} ({})", self.version, digest.get(..12).unwrap_or(digest))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeImage {
    pub version: String,
    pub feature_level: FeatureLevel,
    pub digest: Sha256Digest,
    pub size_bytes: u64,
    pub url: String,
    pub signature: String,
}

impl NodeImage {
    #[must_use]
    pub fn identity(&self) -> ImageIdentity {
        ImageIdentity { version: self.version.clone(), digest: self.digest.clone() }
    }

    #[must_use]
    pub fn is(&self, identity: &ImageIdentity) -> bool {
        self.digest == identity.digest && self.version == identity.version
    }

    #[must_use]
    pub fn signed_payload(&self) -> Vec<u8> {
        signed_payload(&self.version, self.feature_level, &self.digest, self.size_bytes)
    }

    pub fn check(&self, field: &str, error: &mut ValidationError) {
        error.check(
            is_version(&self.version),
            &format!("{field}.version"),
            "must be 1 to 64 characters of letters, digits, '.', '-', '+' or '~'",
        );
        error.check(
            self.url.len() <= LONGEST_URL
                && (self.url.starts_with("https://") || self.url.starts_with("http://")),
            &format!("{field}.url"),
            "must be an http or https URL of at most 2048 characters",
        );
        error.check(
            self.signature.len() == SIGNATURE_HEXADECIMAL_LENGTH
                && self.signature.chars().all(|character| character.is_ascii_hexdigit()),
            &format!("{field}.signature"),
            "must be an Ed25519 signature in 128 hexadecimal digits",
        );
    }
}

#[must_use]
pub fn signed_payload(
    version: &str,
    feature_level: FeatureLevel,
    digest: &Sha256Digest,
    size_bytes: u64,
) -> Vec<u8> {
    format!("{SIGNED_PAYLOAD_HEADER}\n{version}\n{feature_level}\n{digest}\n{size_bytes}\n").into_bytes()
}

#[must_use]
pub fn is_version(version: &str) -> bool {
    (1..=LONGEST_VERSION).contains(&version.len())
        && version
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '+' | '~'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(version: &str) -> NodeImage {
        NodeImage {
            version: version.to_owned(),
            feature_level: FeatureLevel(1),
            digest: "a".repeat(64).parse().unwrap(),
            size_bytes: 1024,
            url: format!("https://images.example/{version}.tar"),
            signature: "0".repeat(128),
        }
    }

    #[test]
    fn the_signature_covers_what_identifies_the_image_but_not_where_it_is_served() {
        let original = image("0.2.0");
        let mirrored = NodeImage { url: String::from("https://mirror.example/node.tar"), ..original.clone() };
        assert_eq!(original.signed_payload(), mirrored.signed_payload());
        let resized = NodeImage { size_bytes: 2048, ..original.clone() };
        assert_ne!(original.signed_payload(), resized.signed_payload());
        let raised = NodeImage { feature_level: FeatureLevel(2), ..original.clone() };
        assert_ne!(original.signed_payload(), raised.signed_payload());
    }

    #[test]
    fn malformed_images_are_refused() {
        let mut error = ValidationError::new();
        image("0.2.0").check("image", &mut error);
        assert_eq!(error.into_result(), Ok(()));
        let mut error = ValidationError::new();
        NodeImage {
            version: String::from("0.2 beta"),
            url: String::from("file:///x"),
            signature: String::from("zz"),
            ..image("0.2.0")
        }
        .check("image", &mut error);
        assert_eq!(error.into_result().unwrap_err().violations.len(), 3);
    }
}
