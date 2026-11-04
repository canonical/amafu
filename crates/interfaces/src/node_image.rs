use amafu_domain::Sha256Digest;
use amafu_domain::features::FeatureLevel;
use amafu_domain::node_image::{NodeImage, signed_payload};
use ring::rand::SystemRandom;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};

const PUBLIC_KEY_LENGTH: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImageTrustError {
    #[error("the image {0} is not signed by a trusted key")]
    Untrusted(String),
    #[error("the image key is malformed: {0}")]
    MalformedKey(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TrustedImageKeys {
    keys: Vec<Vec<u8>>,
}

impl TrustedImageKeys {
    pub fn from_hexadecimal<'key>(
        keys: impl IntoIterator<Item = &'key str>,
    ) -> Result<Self, ImageTrustError> {
        let keys = keys
            .into_iter()
            .map(|key| {
                let bytes = hex::decode(key.trim())
                    .map_err(|error| ImageTrustError::MalformedKey(error.to_string()))?;
                if bytes.len() == PUBLIC_KEY_LENGTH {
                    Ok(bytes)
                } else {
                    Err(ImageTrustError::MalformedKey(format!(
                        "an Ed25519 public key has 32 bytes, not {}",
                        bytes.len()
                    )))
                }
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { keys })
    }

    pub fn verify(&self, image: &NodeImage) -> Result<(), ImageTrustError> {
        let untrusted = || ImageTrustError::Untrusted(image.identity().to_string());
        let signature = hex::decode(&image.signature).map_err(|_| untrusted())?;
        let payload = image.signed_payload();
        let is_trusted = self
            .keys
            .iter()
            .any(|key| UnparsedPublicKey::new(&ED25519, key).verify(&payload, &signature).is_ok());
        if is_trusted { Ok(()) } else { Err(untrusted()) }
    }
}

pub struct ImageSigningKey {
    pair: Ed25519KeyPair,
}

impl ImageSigningKey {
    pub fn generate() -> Result<(Self, Vec<u8>), ImageTrustError> {
        let document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
            .map_err(|error| ImageTrustError::MalformedKey(error.to_string()))?;
        let key = Self::from_pkcs8(document.as_ref())?;
        Ok((key, document.as_ref().to_vec()))
    }

    pub fn from_pkcs8(document: &[u8]) -> Result<Self, ImageTrustError> {
        Ed25519KeyPair::from_pkcs8(document)
            .map(|pair| Self { pair })
            .map_err(|error| ImageTrustError::MalformedKey(error.to_string()))
    }

    #[must_use]
    pub fn public_key_hexadecimal(&self) -> String {
        hex::encode(self.pair.public_key().as_ref())
    }

    #[must_use]
    pub fn sign(
        &self,
        version: &str,
        feature_level: FeatureLevel,
        digest: Sha256Digest,
        size_bytes: u64,
        url: String,
    ) -> NodeImage {
        let signature = self.pair.sign(&signed_payload(version, feature_level, &digest, size_bytes));
        NodeImage {
            version: version.to_owned(),
            feature_level,
            digest,
            size_bytes,
            url,
            signature: hex::encode(signature.as_ref()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed(key: &ImageSigningKey) -> NodeImage {
        key.sign(
            "0.2.0",
            FeatureLevel(1),
            "d".repeat(64).parse().unwrap(),
            4096,
            String::from("https://images.example/node.tar"),
        )
    }

    #[test]
    fn an_image_signed_by_a_trusted_key_is_accepted_and_any_change_is_refused() {
        let (key, _) = ImageSigningKey::generate().unwrap();
        let trusted = TrustedImageKeys::from_hexadecimal([key.public_key_hexadecimal().as_str()]).unwrap();
        let image = signed(&key);
        assert_eq!(trusted.verify(&image), Ok(()));
        let moved = NodeImage { url: String::from("https://mirror.example/node.tar"), ..image.clone() };
        assert_eq!(trusted.verify(&moved), Ok(()));
        let tampered = NodeImage { size_bytes: 1, ..image.clone() };
        assert!(trusted.verify(&tampered).is_err());
        let raised = NodeImage { feature_level: FeatureLevel(9), ..image };
        assert!(trusted.verify(&raised).is_err());
    }

    #[test]
    fn an_image_signed_by_another_key_is_refused() {
        let (key, _) = ImageSigningKey::generate().unwrap();
        let (other, document) = ImageSigningKey::generate().unwrap();
        let trusted = TrustedImageKeys::from_hexadecimal([other.public_key_hexadecimal().as_str()]).unwrap();
        assert!(trusted.verify(&signed(&key)).is_err());
        let reloaded = ImageSigningKey::from_pkcs8(&document).unwrap();
        assert_eq!(trusted.verify(&signed(&reloaded)), Ok(()));
        assert!(TrustedImageKeys::from_hexadecimal(["abcd"]).is_err());
    }
}
