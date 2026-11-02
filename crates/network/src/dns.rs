use amafu_interfaces::{NodeNetworkState, SegmentKey};

const HEADER_BYTES: usize = 12;
const MAXIMUM_NAME_BYTES: usize = 255;
const RECORD_TYPE_A: u16 = 1;
const RECORD_TYPE_ANY: u16 = 255;
const CLASS_INTERNET: u16 = 1;
const ANSWER_TIME_TO_LIVE: u32 = 30;
const FLAG_RESPONSE: u16 = 0x8000;
const FLAG_AUTHORITATIVE: u16 = 0x0400;
const FLAG_RECURSION_DESIRED: u16 = 0x0100;
const FLAG_RECURSION_AVAILABLE: u16 = 0x0080;
const OPCODE_MASK: u16 = 0x7800;
const RESPONSE_CODE_NAME_ERROR: u16 = 3;
const POINTER_TO_QUESTION: u16 = 0xc00c;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub identifier: u16,
    pub wants_recursion: bool,
    pub name: String,
    pub record_type: u16,
    pub record_class: u16,
    question: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the DNS query is malformed: {0}")]
pub struct MalformedQuery(&'static str);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Answer(Vec<u8>),
    Forward,
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*bytes.get(offset)?, *bytes.get(offset + 1)?]))
}

impl Query {
    pub fn parse(bytes: &[u8]) -> Result<Self, MalformedQuery> {
        if bytes.len() < HEADER_BYTES {
            return Err(MalformedQuery("it is shorter than a header"));
        }
        let identifier = read_u16(bytes, 0).ok_or(MalformedQuery("no identifier"))?;
        let flags = read_u16(bytes, 2).ok_or(MalformedQuery("no flags"))?;
        if flags & FLAG_RESPONSE != 0 || flags & OPCODE_MASK != 0 {
            return Err(MalformedQuery("only standard queries are answered"));
        }
        if read_u16(bytes, 4) != Some(1) {
            return Err(MalformedQuery("exactly one question is required"));
        }
        let mut offset = HEADER_BYTES;
        let mut labels: Vec<String> = Vec::new();
        loop {
            let length = usize::from(*bytes.get(offset).ok_or(MalformedQuery("the name runs past the end"))?);
            offset += 1;
            if length == 0 {
                break;
            }
            if length > 63 {
                return Err(MalformedQuery(
                    "labels are at most 63 bytes and compression is not allowed here",
                ));
            }
            let label =
                bytes.get(offset..offset + length).ok_or(MalformedQuery("a label runs past the end"))?;
            if !label.iter().all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-' || *byte == b'_') {
                return Err(MalformedQuery("labels must be letters, digits, hyphens or underscores"));
            }
            labels.push(String::from_utf8_lossy(label).to_ascii_lowercase());
            offset += length;
            if offset - HEADER_BYTES > MAXIMUM_NAME_BYTES {
                return Err(MalformedQuery("the name is longer than 255 bytes"));
            }
        }
        let record_type = read_u16(bytes, offset).ok_or(MalformedQuery("no record type"))?;
        let record_class = read_u16(bytes, offset + 2).ok_or(MalformedQuery("no record class"))?;
        Ok(Self {
            identifier,
            wants_recursion: flags & FLAG_RECURSION_DESIRED != 0,
            name: labels.join("."),
            record_type,
            record_class,
            question: bytes[HEADER_BYTES..offset + 4].to_vec(),
        })
    }
}

fn response(query: &Query, response_code: u16, addresses: &[[u8; 4]]) -> Vec<u8> {
    let mut flags = FLAG_RESPONSE | FLAG_AUTHORITATIVE | FLAG_RECURSION_AVAILABLE | response_code;
    if query.wants_recursion {
        flags |= FLAG_RECURSION_DESIRED;
    }
    let answer_count = u16::try_from(addresses.len()).unwrap_or(u16::MAX);
    let mut bytes = Vec::with_capacity(HEADER_BYTES + query.question.len() + addresses.len() * 16);
    for field in [query.identifier, flags, 1, answer_count, 0, 0] {
        bytes.extend_from_slice(&field.to_be_bytes());
    }
    bytes.extend_from_slice(&query.question);
    for address in addresses {
        bytes.extend_from_slice(&POINTER_TO_QUESTION.to_be_bytes());
        bytes.extend_from_slice(&RECORD_TYPE_A.to_be_bytes());
        bytes.extend_from_slice(&CLASS_INTERNET.to_be_bytes());
        bytes.extend_from_slice(&ANSWER_TIME_TO_LIVE.to_be_bytes());
        bytes.extend_from_slice(&4_u16.to_be_bytes());
        bytes.extend_from_slice(address);
    }
    bytes
}

#[must_use]
pub fn answer(query: &Query, segment: &SegmentKey, state: &NodeNetworkState) -> Decision {
    let Some(network) = state.segment(segment) else {
        return Decision::Forward;
    };
    let domain = network.domain.to_ascii_lowercase();
    let short_name = match query.name.strip_suffix(&format!(".{domain}")) {
        Some(short) => short.to_owned(),
        None if query.name.contains('.') => return Decision::Forward,
        None => query.name.clone(),
    };
    let found: Vec<[u8; 4]> = state
        .ports
        .iter()
        .filter(|port| &port.segment == segment && port.hostname.eq_ignore_ascii_case(&short_name))
        .map(|port| port.address.octets())
        .collect();
    if found.is_empty() {
        return Decision::Answer(response(query, RESPONSE_CODE_NAME_ERROR, &[]));
    }
    let wants_address =
        query.record_class == CLASS_INTERNET && matches!(query.record_type, RECORD_TYPE_A | RECORD_TYPE_ANY);
    Decision::Answer(response(query, 0, if wants_address { &found } else { &[] }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dhcp::tests::{segment, state};

    fn query(name: &str, record_type: u16) -> Vec<u8> {
        let mut bytes = Vec::new();
        for field in [0xbeef_u16, FLAG_RECURSION_DESIRED, 1, 0, 0, 0] {
            bytes.extend_from_slice(&field.to_be_bytes());
        }
        for label in name.split('.') {
            bytes.push(u8::try_from(label.len()).unwrap());
            bytes.extend_from_slice(label.as_bytes());
        }
        bytes.push(0);
        bytes.extend_from_slice(&record_type.to_be_bytes());
        bytes.extend_from_slice(&CLASS_INTERNET.to_be_bytes());
        bytes
    }

    fn decide(name: &str, record_type: u16) -> Decision {
        answer(&Query::parse(&query(name, record_type)).unwrap(), &segment(), &state())
    }

    #[test]
    fn local_names_resolve_with_or_without_the_domain() {
        for name in ["web-1", "WEB-1", "web-1.default.project-a.internal"] {
            let Decision::Answer(bytes) = decide(name, RECORD_TYPE_A) else { panic!("{name} was forwarded") };
            assert_eq!(read_u16(&bytes, 6), Some(1), "{name}");
            assert_eq!(&bytes[bytes.len() - 4..], &[10, 1, 0, 2]);
            assert_eq!(read_u16(&bytes, 0), Some(0xbeef));
        }
    }

    #[test]
    fn unknown_local_names_do_not_exist_and_other_names_are_forwarded() {
        let Decision::Answer(bytes) = decide("nobody.default.project-a.internal", RECORD_TYPE_A) else {
            panic!("forwarded")
        };
        assert_eq!(read_u16(&bytes, 2).map(|flags| flags & 0x000f), Some(RESPONSE_CODE_NAME_ERROR));
        assert_eq!(decide("archive.ubuntu.com", RECORD_TYPE_A), Decision::Forward);
    }

    #[test]
    fn other_record_types_of_local_names_get_an_empty_answer() {
        let Decision::Answer(bytes) = decide("web-1", 28) else { panic!("forwarded") };
        assert_eq!(read_u16(&bytes, 6), Some(0));
    }

    #[test]
    fn malformed_queries_are_refused() {
        let good = query("web-1", RECORD_TYPE_A);
        assert!(Query::parse(&good[..5]).is_err());
        let mut response_bit = good.clone();
        response_bit[2] |= 0x80;
        assert!(Query::parse(&response_bit).is_err());
        let mut compressed = good.clone();
        compressed[12] = 0xc0;
        assert!(Query::parse(&compressed).is_err());
        assert!(Query::parse(&good[..good.len() - 2]).is_err());
    }
}
