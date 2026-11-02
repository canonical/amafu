use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

const SHA256_HEXADECIMAL_LENGTH: usize = 64;
const DIGEST_PREFIX: &str = "sha256:";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{value:?} is not valid: {reason}")]
pub struct InvalidReference {
    pub value: String,
    pub reason: &'static str,
}

fn invalid(value: &str, reason: &'static str) -> InvalidReference {
    InvalidReference { value: value.to_owned(), reason }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sha256Digest(String);

impl Sha256Digest {
    #[must_use]
    pub fn hexadecimal(&self) -> &str {
        &self.0
    }
}

impl FromStr for Sha256Digest {
    type Err = InvalidReference;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let hexadecimal = value.strip_prefix(DIGEST_PREFIX).unwrap_or(value);
        let is_valid = hexadecimal.len() == SHA256_HEXADECIMAL_LENGTH
            && hexadecimal
                .chars()
                .all(|character| character.is_ascii_digit() || ('a'..='f').contains(&character));
        if is_valid {
            Ok(Self(hexadecimal.to_owned()))
        } else {
            Err(invalid(value, "it must be 64 lowercase hexadecimal digits"))
        }
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{DIGEST_PREFIX}{}", self.0)
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OciReference {
    pub registry: String,
    pub repository: String,
    pub digest: Sha256Digest,
}

fn is_registry_host(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value.chars().all(|character| character.is_ascii_alphanumeric() || ".-:".contains(character))
        && (value.contains('.') || value.contains(':') || value == "localhost")
}

fn is_repository_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value.split('/').all(|component| {
            !component.is_empty()
                && component.chars().all(|character| {
                    character.is_ascii_lowercase() || character.is_ascii_digit() || "._-".contains(character)
                })
        })
}

impl FromStr for OciReference {
    type Err = InvalidReference;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (name, digest) =
            value.split_once('@').ok_or_else(|| invalid(value, "it must be pinned with @sha256:<digest>"))?;
        let (registry, repository) =
            name.split_once('/').ok_or_else(|| invalid(value, "it must name a registry and a repository"))?;
        if !is_registry_host(registry) {
            return Err(invalid(value, "the registry must be a host name such as docker.io"));
        }
        if !is_repository_path(repository) {
            return Err(invalid(value, "the repository must be lowercase path components"));
        }
        if !digest.starts_with(DIGEST_PREFIX) {
            return Err(invalid(value, "the digest must start with sha256:"));
        }
        Ok(Self { registry: registry.to_owned(), repository: repository.to_owned(), digest: digest.parse()? })
    }
}

impl fmt::Display for OciReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}@{}", self.registry, self.repository, self.digest)
    }
}

impl Serialize for OciReference {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for OciReference {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const DIGEST: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn a_pinned_reference_parses_and_prints() {
        let text = format!("docker.io/library/busybox@{DIGEST}");
        let reference: OciReference = text.parse().unwrap();
        assert_eq!(reference.registry, "docker.io");
        assert_eq!(reference.repository, "library/busybox");
        assert_eq!(reference.to_string(), text);
    }

    #[test]
    fn unpinned_or_malformed_references_are_refused() {
        for value in [
            String::from("docker.io/library/busybox:latest"),
            format!("busybox@{DIGEST}"),
            format!("docker.io/Library/busybox@{DIGEST}"),
            String::from("docker.io/library/busybox@sha256:abc"),
            format!("docker.io//busybox@{DIGEST}"),
        ] {
            assert!(value.parse::<OciReference>().is_err(), "{value}");
        }
    }

    proptest! {
        #[test]
        fn arbitrary_references_never_panic(value in ".*") {
            let _ = value.parse::<OciReference>();
            let _ = value.parse::<Sha256Digest>();
        }
    }
}
