use amafu_interfaces::{WorkloadSpecification, WorkloadState};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
pub const AGENT_PORT: u32 = 1024;
pub const MAXIMUM_MESSAGE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Request {
    Status,
    Start { workload: WorkloadSpecification },
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Reply {
    Status { workload: WorkloadState },
    Started,
    Stopping,
    Refused { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<T> {
    version: u32,
    message: T,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    #[error("the message is larger than 64 KiB")]
    TooLarge,
    #[error("protocol version {0} is not spoken here")]
    UnsupportedVersion(u32),
    #[error("the message is malformed: {0}")]
    Malformed(String),
}

#[must_use]
pub fn encode<T: Serialize>(message: &T) -> Vec<u8> {
    let mut line = serde_json::to_vec(&Envelope { version: PROTOCOL_VERSION, message }).unwrap_or_default();
    line.push(b'\n');
    line
}

pub fn decode<T: DeserializeOwned>(line: &[u8]) -> Result<T, ProtocolError> {
    if line.len() > MAXIMUM_MESSAGE_BYTES {
        return Err(ProtocolError::TooLarge);
    }
    let envelope: Envelope<serde_json::Value> =
        serde_json::from_slice(line).map_err(|error| ProtocolError::Malformed(error.to_string()))?;
    if envelope.version != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion(envelope.version));
    }
    serde_json::from_value(envelope.message).map_err(|error| ProtocolError::Malformed(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use proptest::prelude::*;

    use super::*;

    #[test]
    fn messages_round_trip_as_single_lines() {
        let request = Request::Start {
            workload: WorkloadSpecification {
                arguments: vec![String::from("httpd"), String::from("-f")],
                environment: BTreeMap::from([(String::from("PORT"), String::from("80"))]),
                working_directory: None,
                user: None,
                hostname: String::from("web-1"),
            },
        };
        let line = encode(&request);
        assert_eq!(line.last(), Some(&b'\n'));
        assert!(!line[..line.len() - 1].contains(&b'\n'));
        assert_eq!(decode::<Request>(&line[..line.len() - 1]), Ok(request));
        let reply = Reply::Status { workload: WorkloadState::Exited { code: 3 } };
        assert_eq!(decode::<Reply>(&encode(&reply)), Ok(reply));
    }

    #[test]
    fn other_versions_and_oversized_messages_are_refused() {
        assert_eq!(
            decode::<Reply>(br#"{"version":2,"message":{"type":"started"}}"#),
            Err(ProtocolError::UnsupportedVersion(2))
        );
        assert_eq!(decode::<Reply>(&vec![b' '; MAXIMUM_MESSAGE_BYTES + 1]), Err(ProtocolError::TooLarge));
        assert!(decode::<Reply>(br#"{"version":1,"message":{"type":"launched"}}"#).is_err());
    }

    proptest! {
        #[test]
        fn arbitrary_replies_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            let _ = decode::<Reply>(&bytes);
            let _ = decode::<Request>(&bytes);
        }
    }
}
