use uuid::Uuid;

pub trait Identifiers: Send + Sync {
    fn generate(&self) -> Uuid;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RandomIdentifiers;

impl Identifiers for RandomIdentifiers {
    fn generate(&self) -> Uuid {
        Uuid::new_v4()
    }
}
