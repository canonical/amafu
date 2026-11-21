use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{Ed25519KeyPair, KeyPair};

const KEY_TYPE: &str = "ssh-ed25519";
const MAGIC: &[u8] = b"openssh-key-v1\0";
const LINE_WIDTH: usize = 70;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum KeyPairError {
    #[error("the system has no randomness for an SSH key")]
    NoRandomness,
    #[error("an SSH key seed was refused")]
    RefusedSeed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshKeyPair {
    pub public_key: String,
    pub private_key: String,
}

fn put_string(buffer: &mut Vec<u8>, value: &[u8]) {
    let length = u32::try_from(value.len()).unwrap_or(u32::MAX);
    buffer.extend_from_slice(&length.to_be_bytes());
    buffer.extend_from_slice(value);
}

fn public_blob(public: &[u8]) -> Vec<u8> {
    let mut blob = Vec::new();
    put_string(&mut blob, KEY_TYPE.as_bytes());
    put_string(&mut blob, public);
    blob
}

impl SshKeyPair {
    pub fn generate(comment: &str) -> Result<Self, KeyPairError> {
        let random = SystemRandom::new();
        let mut seed = [0_u8; 32];
        random.fill(&mut seed).map_err(|_| KeyPairError::NoRandomness)?;
        let mut check = [0_u8; 4];
        random.fill(&mut check).map_err(|_| KeyPairError::NoRandomness)?;
        Self::from_seed(&seed, check, comment)
    }

    fn from_seed(seed: &[u8; 32], check: [u8; 4], comment: &str) -> Result<Self, KeyPairError> {
        let key = Ed25519KeyPair::from_seed_unchecked(seed).map_err(|_| KeyPairError::RefusedSeed)?;
        let public = key.public_key().as_ref();
        let blob = public_blob(public);
        let mut private = Vec::new();
        private.extend_from_slice(&check);
        private.extend_from_slice(&check);
        put_string(&mut private, KEY_TYPE.as_bytes());
        put_string(&mut private, public);
        put_string(&mut private, &[seed.as_slice(), public].concat());
        put_string(&mut private, comment.as_bytes());
        let mut padding = 1_u8;
        while private.len() % 8 != 0 {
            private.push(padding);
            padding += 1;
        }
        let mut document = MAGIC.to_vec();
        put_string(&mut document, b"none");
        put_string(&mut document, b"none");
        put_string(&mut document, b"");
        document.extend_from_slice(&1_u32.to_be_bytes());
        put_string(&mut document, &blob);
        put_string(&mut document, &private);
        let encoded = STANDARD.encode(document);
        let lines: Vec<&str> = encoded
            .as_bytes()
            .chunks(LINE_WIDTH)
            .map(|line| std::str::from_utf8(line).unwrap_or_default())
            .collect();
        Ok(Self {
            public_key: format!("{KEY_TYPE} {} {comment}", STANDARD.encode(blob)),
            private_key: format!(
                "-----BEGIN OPENSSH PRIVATE KEY-----\n{}\n-----END OPENSSH PRIVATE KEY-----\n",
                lines.join("\n")
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_pair_is_written_in_the_openssh_formats() {
        let pair = SshKeyPair::from_seed(&[7; 32], [1, 2, 3, 4], "amafu-bootstrap").unwrap();
        let fields: Vec<&str> = pair.public_key.split(' ').collect();
        assert_eq!(fields[0], "ssh-ed25519");
        assert_eq!(fields[2], "amafu-bootstrap");
        assert_eq!(STANDARD.decode(fields[1]).unwrap().len(), 4 + 11 + 4 + 32);
        let body: String = pair.private_key.lines().filter(|line| !line.starts_with("-----")).collect();
        let document = STANDARD.decode(body).unwrap();
        assert!(document.starts_with(MAGIC));
        assert!(pair.private_key.lines().all(|line| line.len() <= LINE_WIDTH));
    }

    #[test]
    fn every_key_is_new() {
        assert_ne!(SshKeyPair::generate("a").unwrap(), SshKeyPair::generate("a").unwrap());
    }
}
