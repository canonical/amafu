use ring::hmac;
use sha2::{Digest, Sha256};

pub const UNSIGNED_EMPTY_BODY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const ALGORITHM: &str = "AWS4-HMAC-SHA256";
const SERVICE: &str = "s3";
const TERMINATOR: &str = "aws4_request";

pub struct SignedRequest<'request> {
    pub method: &'request str,
    pub path: &'request str,
    pub headers: Vec<(String, String)>,
    pub body_digest: &'request str,
}

pub struct Credentials<'credentials> {
    pub access_key: &'credentials str,
    pub secret_key: &'credentials str,
    pub region: &'credentials str,
}

fn keyed(key: &[u8], message: &str) -> Vec<u8> {
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), message.as_bytes()).as_ref().to_vec()
}

#[must_use]
pub fn digest_of(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[must_use]
pub fn authorization(request: &SignedRequest<'_>, credentials: &Credentials<'_>, timestamp: &str) -> String {
    let date = timestamp.get(..8).unwrap_or(timestamp);
    let mut headers: Vec<(String, String)> = request
        .headers
        .iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    headers.sort();
    let names: Vec<&str> = headers.iter().map(|(name, _)| name.as_str()).collect();
    let signed_headers = names.join(";");
    let lines: Vec<String> = headers.iter().map(|(name, value)| format!("{name}:{value}\n")).collect();
    let canonical = format!(
        "{}\n{}\n\n{}\n{signed_headers}\n{}",
        request.method,
        request.path,
        lines.concat(),
        request.body_digest
    );
    let scope = format!("{date}/{}/{SERVICE}/{TERMINATOR}", credentials.region);
    let to_sign = format!("{ALGORITHM}\n{timestamp}\n{scope}\n{}", digest_of(canonical.as_bytes()));
    let date_key = keyed(format!("AWS4{}", credentials.secret_key).as_bytes(), date);
    let region_key = keyed(&date_key, credentials.region);
    let service_key = keyed(&region_key, SERVICE);
    let signing_key = keyed(&service_key, TERMINATOR);
    let signature = hex::encode(keyed(&signing_key, &to_sign));
    format!(
        "{ALGORITHM} Credential={}/{scope},SignedHeaders={signed_headers},Signature={signature}",
        credentials.access_key
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_signature_matches_the_published_example() {
        let request = SignedRequest {
            method: "GET",
            path: "/test.txt",
            headers: vec![
                (String::from("Host"), String::from("examplebucket.s3.amazonaws.com")),
                (String::from("Range"), String::from("bytes=0-9")),
                (String::from("x-amz-content-sha256"), UNSIGNED_EMPTY_BODY.to_owned()),
                (String::from("x-amz-date"), String::from("20130524T000000Z")),
            ],
            body_digest: UNSIGNED_EMPTY_BODY,
        };
        let credentials = Credentials {
            access_key: "AKIAIOSFODNN7EXAMPLE",
            secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            region: "us-east-1",
        };
        assert_eq!(
            authorization(&request, &credentials, "20130524T000000Z"),
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request,SignedHeaders=host;range;x-amz-content-sha256;x-amz-date,Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }
}
