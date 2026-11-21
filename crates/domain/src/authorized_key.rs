use std::fmt;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use sha2::{Digest, Sha256};

pub const MAXIMUM_KEY_LENGTH: usize = 16 * 1024;
pub const MINIMUM_RSA_BITS: usize = 2048;

const RSA: &str = "ssh-rsa";
const ALGORITHMS: &[&str] = &[
    "ssh-ed25519",
    "sk-ssh-ed25519@openssh.com",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
    "sk-ecdsa-sha2-nistp256@openssh.com",
    RSA,
];
const PRIVATE_KEY_MARKER: &str = "PRIVATE KEY";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    #[error("the key is empty")]
    Empty,
    #[error(
        "this is a private key; paste the public key (the .pub file) instead, and keep the private key to yourself"
    )]
    PrivateKey,
    #[error("give one key per line")]
    SeveralLines,
    #[error("the key is longer than {MAXIMUM_KEY_LENGTH} characters")]
    TooLong,
    #[error("{0:?} is not a supported key type")]
    UnsupportedAlgorithm(String),
    #[error("the key is malformed: {0}")]
    Malformed(&'static str),
    #[error(
        "an RSA key of {0} bits is too weak; use at least {MINIMUM_RSA_BITS} bits, or better an Ed25519 key"
    )]
    WeakRsa(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedKey {
    algorithm: String,
    blob: Vec<u8>,
    comment: Option<String>,
}

struct Reader<'a> {
    remaining: &'a [u8],
}

impl<'a> Reader<'a> {
    fn string(&mut self) -> Result<&'a [u8], KeyError> {
        let (length, rest) =
            self.remaining.split_at_checked(4).ok_or(KeyError::Malformed("a field is cut short"))?;
        let length = u32::from_be_bytes([length[0], length[1], length[2], length[3]]) as usize;
        let (value, rest) =
            rest.split_at_checked(length).ok_or(KeyError::Malformed("a field is cut short"))?;
        self.remaining = rest;
        Ok(value)
    }
}

fn significant_bits(integer: &[u8]) -> usize {
    let trimmed: &[u8] = match integer.iter().position(|byte| *byte != 0) {
        Some(first) => &integer[first..],
        None => &[],
    };
    trimmed.first().map_or(0, |first| (trimmed.len() - 1) * 8 + (8 - first.leading_zeros() as usize))
}

impl AuthorizedKey {
    pub fn parse(text: &str) -> Result<Self, KeyError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(KeyError::Empty);
        }
        if text.contains(PRIVATE_KEY_MARKER) {
            return Err(KeyError::PrivateKey);
        }
        if text.len() > MAXIMUM_KEY_LENGTH {
            return Err(KeyError::TooLong);
        }
        if text.lines().count() > 1 {
            return Err(KeyError::SeveralLines);
        }
        let mut fields = text.split_whitespace();
        let algorithm = fields.next().ok_or(KeyError::Empty)?;
        if !ALGORITHMS.contains(&algorithm) {
            return Err(KeyError::UnsupportedAlgorithm(algorithm.to_owned()));
        }
        let encoded = fields.next().ok_or(KeyError::Malformed("the key data is missing"))?;
        let blob = STANDARD.decode(encoded).map_err(|_| KeyError::Malformed("the key data is not base64"))?;
        let comment = fields.collect::<Vec<_>>().join(" ");
        let key =
            Self { algorithm: algorithm.to_owned(), blob, comment: (!comment.is_empty()).then_some(comment) };
        key.check()?;
        Ok(key)
    }

    fn check(&self) -> Result<(), KeyError> {
        let mut reader = Reader { remaining: &self.blob };
        if reader.string()? != self.algorithm.as_bytes() {
            return Err(KeyError::Malformed("the key data names another key type"));
        }
        if self.algorithm == RSA {
            reader.string()?;
            let bits = significant_bits(reader.string()?);
            if bits < MINIMUM_RSA_BITS {
                return Err(KeyError::WeakRsa(bits));
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn algorithm(&self) -> &str {
        &self.algorithm
    }

    #[must_use]
    pub fn comment(&self) -> Option<&str> {
        self.comment.as_deref()
    }

    #[must_use]
    pub fn fingerprint(&self) -> String {
        format!("SHA256:{}", STANDARD_NO_PAD.encode(Sha256::digest(&self.blob)))
    }

    #[must_use]
    pub fn line(&self) -> String {
        let data = STANDARD.encode(&self.blob);
        match &self.comment {
            Some(comment) => format!("{} {data} {comment}", self.algorithm),
            None => format!("{} {data}", self.algorithm),
        }
    }
}

impl fmt::Display for AuthorizedKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.line())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ED25519: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOpKbGPinFIKvvVQexMuxfmVR3auvr57kkIe6mkURtIs amafu-bootstrap";

    fn string(value: &[u8]) -> Vec<u8> {
        [&u32::try_from(value.len()).unwrap().to_be_bytes()[..], value].concat()
    }

    fn rsa(bits: usize) -> String {
        let modulus = [vec![0_u8], vec![0x80], vec![0_u8; bits / 8 - 1]].concat();
        let blob = [string(b"ssh-rsa"), string(&[1, 0, 1]), string(&modulus)].concat();
        format!("ssh-rsa {}", STANDARD.encode(blob))
    }

    #[test]
    fn a_public_key_keeps_its_comment_and_has_ssh_keygens_fingerprint() {
        let key = AuthorizedKey::parse(&format!("  {ED25519}\n")).unwrap();
        assert_eq!(key.algorithm(), "ssh-ed25519");
        assert_eq!(key.comment(), Some("amafu-bootstrap"));
        assert_eq!(key.fingerprint(), "SHA256:z/fSv0Z0RZS+Lccbc6ZoOWwt/fbj1VFJGSBKQbo4icE");
        assert_eq!(key.line(), ED25519);
    }

    #[test]
    fn private_keys_and_strangers_are_refused_with_a_reason() {
        assert_eq!(AuthorizedKey::parse("-----BEGIN OPENSSH PRIVATE KEY-----"), Err(KeyError::PrivateKey));
        assert_eq!(AuthorizedKey::parse(""), Err(KeyError::Empty));
        assert_eq!(
            AuthorizedKey::parse("ssh-dss AAAA"),
            Err(KeyError::UnsupportedAlgorithm(String::from("ssh-dss")))
        );
        assert!(matches!(AuthorizedKey::parse("ssh-ed25519 !!!"), Err(KeyError::Malformed(_))));
        assert_eq!(AuthorizedKey::parse(&format!("{ED25519}\n{ED25519}")), Err(KeyError::SeveralLines));
        let mislabelled = ED25519.replacen("ssh-ed25519", "ecdsa-sha2-nistp256", 1);
        assert!(matches!(AuthorizedKey::parse(&mislabelled), Err(KeyError::Malformed(_))));
    }

    #[test]
    fn rsa_keys_need_enough_bits() {
        assert_eq!(AuthorizedKey::parse(&rsa(1024)), Err(KeyError::WeakRsa(1024)));
        assert!(AuthorizedKey::parse(&rsa(2048)).is_ok());
    }
}
