use amafu_domain::kinds::node::StoreHealth;

use crate::error::ActionError;

#[async_trait::async_trait]
pub trait ConsensusMembership: Send + Sync {
    fn health(&self) -> Option<StoreHealth>;

    async fn hand_off_leadership(&self) -> Result<(), ActionError>;

    async fn close(&self);

    fn is_stable(&self) -> bool {
        self.health().is_none_or(|health| health.is_stable)
    }
}

pub struct NotAVoter;

#[async_trait::async_trait]
impl ConsensusMembership for NotAVoter {
    fn health(&self) -> Option<StoreHealth> {
        None
    }

    async fn hand_off_leadership(&self) -> Result<(), ActionError> {
        Ok(())
    }

    async fn close(&self) {}
}
