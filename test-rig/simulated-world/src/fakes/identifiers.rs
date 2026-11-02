use std::sync::Mutex;

use amafu_interfaces::Identifiers;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use uuid::Uuid;

use crate::lock::lock;

pub struct SeededIdentifiers {
    random: Mutex<ChaCha8Rng>,
}

impl SeededIdentifiers {
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self { random: Mutex::new(ChaCha8Rng::seed_from_u64(seed)) }
    }
}

impl Identifiers for SeededIdentifiers {
    fn generate(&self) -> Uuid {
        let mut bytes = [0_u8; 16];
        lock(&self.random).fill_bytes(&mut bytes);
        uuid::Builder::from_random_bytes(bytes).into_uuid()
    }
}
