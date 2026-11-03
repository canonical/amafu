use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

const MAXIMUM_LENGTH: usize = 63;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{value:?} is not a valid name: {reason}")]
pub struct InvalidName {
    pub value: String,
    pub reason: &'static str,
}

fn check_label(value: &str) -> Result<(), InvalidName> {
    let invalid = |reason| Err(InvalidName { value: value.to_owned(), reason });
    let Some(first) = value.chars().next() else {
        return invalid("it is empty");
    };
    if value.len() > MAXIMUM_LENGTH {
        return invalid("it is longer than 63 characters");
    }
    if !first.is_ascii_lowercase() {
        return invalid("it must start with a lowercase letter");
    }
    if value.ends_with('-') {
        return invalid("it must not end with a hyphen");
    }
    if !value
        .chars()
        .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-')
    {
        return invalid("it may contain only lowercase letters, digits and hyphens");
    }
    Ok(())
}

macro_rules! label_name {
    ($type_name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $type_name(String);

        impl $type_name {
            pub fn new(value: impl Into<String>) -> Result<Self, InvalidName> {
                let value = value.into();
                check_label(&value)?;
                Ok(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $type_name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl std::str::FromStr for $type_name {
            type Err = InvalidName;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $type_name {
            type Error = InvalidName;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl AsRef<str> for $type_name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $type_name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

label_name!(ObjectName);
label_name!(ProjectName);
label_name!(NodeName);
label_name!(ZoneName);

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn accepts_simple_labels() {
        for value in ["web", "web-1", "a1-b2", "node-0"] {
            assert!(ObjectName::new(value).is_ok(), "{value}");
        }
    }

    #[test]
    fn rejects_malformed_labels() {
        let too_long = "a".repeat(64);
        for value in ["", "1web", "Web", "web-", "web_1", "web.1", "-web", too_long.as_str()] {
            assert!(ObjectName::new(value).is_err(), "{value:?}");
        }
    }

    #[test]
    fn deserialization_validates() {
        let parsed: Result<NodeName, _> = serde_json::from_str("\"Bad Name\"");
        assert!(parsed.is_err());
    }

    proptest! {
        #[test]
        fn valid_names_round_trip(value in "[a-z]([a-z0-9-]{0,61}[a-z0-9])?") {
            let name = ProjectName::new(value.clone()).map_err(|error| TestCaseError::fail(error.to_string()))?;
            prop_assert_eq!(name.as_str(), value.as_str());
            let encoded = serde_json::to_string(&name).map_err(|error| TestCaseError::fail(error.to_string()))?;
            let decoded: ProjectName = serde_json::from_str(&encoded).map_err(|error| TestCaseError::fail(error.to_string()))?;
            prop_assert_eq!(decoded, name);
        }

        #[test]
        fn arbitrary_strings_never_panic(value in ".*") {
            let _ = ObjectName::new(value);
        }
    }
}
