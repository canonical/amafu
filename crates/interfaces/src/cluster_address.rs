#[async_trait::async_trait]
pub trait ClusterAddressClaim: Send + Sync {
    fn holds(&self) -> bool;

    async fn step_aside(&self);
}

pub struct NoClusterAddress;

#[async_trait::async_trait]
impl ClusterAddressClaim for NoClusterAddress {
    fn holds(&self) -> bool {
        false
    }

    async fn step_aside(&self) {}
}
