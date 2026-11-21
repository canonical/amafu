use std::io;

pub const SEND_MIGRATION: &str = "/api/v1/vm.send-migration";
pub const RECEIVE_MIGRATION: &str = "/api/v1/vm.receive-migration";

const HEADER_END: &[u8] = b"\r\n\r\n";
const CONTENT_LENGTH: &str = "content-length:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: String,
    pub body: String,
}

impl Response {
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.status.starts_with('2')
    }
}

#[must_use]
pub fn put_request(path: &str, body: &str) -> String {
    format!(
        "PUT {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

fn header_end(received: &[u8]) -> Option<usize> {
    received
        .windows(HEADER_END.len())
        .position(|window| window == HEADER_END)
        .map(|position| position + HEADER_END.len())
}

fn content_length(headers: &str) -> usize {
    headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase().strip_prefix(CONTENT_LENGTH).map(|value| value.trim().to_owned())
        })
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

fn response(received: &[u8], header_end: usize) -> Response {
    let headers = String::from_utf8_lossy(&received[..header_end]);
    Response {
        status: headers.split_whitespace().nth(1).unwrap_or_default().to_owned(),
        body: String::from_utf8_lossy(&received[header_end..]).into_owned(),
    }
}

#[must_use]
pub fn complete(received: &[u8]) -> Option<Response> {
    let end = header_end(received)?;
    let headers = String::from_utf8_lossy(&received[..end]);
    (received.len() >= end + content_length(&headers)).then(|| response(received, end))
}

pub fn closed(received: &[u8]) -> io::Result<Response> {
    header_end(received)
        .map(|end| response(received, end))
        .ok_or_else(|| io::Error::other("the VMM closed its API connection early"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_response_is_complete_once_its_body_has_arrived() {
        let headers = b"HTTP/1.1 400 Bad Request\r\nContent-Length: 5\r\n\r\n";
        assert_eq!(complete(headers), None);
        let whole = [headers.as_slice(), b"wrong"].concat();
        let answer = complete(&whole).unwrap();
        assert_eq!(answer.status, "400");
        assert_eq!(answer.body, "wrong");
        assert!(!answer.is_success());
        assert!(complete(b"HTTP/1.1 204 No Content\r\n\r\n").unwrap().is_success());
    }

    #[test]
    fn a_connection_closed_before_the_headers_is_an_error() {
        assert!(closed(b"HTTP/1.1 20").is_err());
        assert_eq!(closed(b"HTTP/1.1 200 OK\r\n\r\npartial").unwrap().body, "partial");
    }

    #[test]
    fn a_put_request_declares_its_length() {
        assert!(put_request("/api/v1/vm.receive-migration", "{}").ends_with("Content-Length: 2\r\n\r\n{}"));
    }
}
