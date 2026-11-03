use futures::future::BoxFuture;

use crate::store::{Fence, StoreError};

pub struct HeldLease {
    pub fence: Fence,
    lost: BoxFuture<'static, ()>,
}

impl HeldLease {
    #[must_use]
    pub fn new(fence: Fence, lost: BoxFuture<'static, ()>) -> Self {
        Self { fence, lost }
    }

    pub async fn lost(self) {
        self.lost.await;
    }

    pub fn into_parts(self) -> (Fence, BoxFuture<'static, ()>) {
        (self.fence, self.lost)
    }
}

#[async_trait::async_trait]
pub trait Leases: Send + Sync {
    async fn acquire(&self, name: &str) -> Result<HeldLease, StoreError>;
}
