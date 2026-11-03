use amafu_interfaces::{Fence, HeldLease, Leases, StoreError};
use futures::channel::{mpsc, oneshot};
use futures::{FutureExt, StreamExt};
use quorium::{Lease, Quorium};

#[derive(Clone)]
pub struct QuoriumLeases {
    node: Quorium,
}

impl QuoriumLeases {
    #[must_use]
    pub fn new(node: Quorium) -> Self {
        Self { node }
    }
}

#[async_trait::async_trait]
impl Leases for QuoriumLeases {
    async fn acquire(&self, name: &str) -> Result<HeldLease, StoreError> {
        let (grants, mut granted) = mpsc::unbounded::<(u64, oneshot::Receiver<()>)>();
        let singleton = self.node.singleton(name, move |lease: Lease| {
            let grants = grants.clone();
            async move {
                let (held, lost) = oneshot::channel::<()>();
                if grants.unbounded_send((lease.token, lost)).is_ok() {
                    futures::future::pending::<()>().await;
                }
                drop(held);
            }
        });
        let (token, lost) = granted
            .next()
            .await
            .ok_or_else(|| StoreError::Unavailable(String::from("the lease service stopped")))?;
        let lost = async move {
            let _singleton = singleton;
            let _ = lost.await;
        }
        .boxed();
        Ok(HeldLease::new(Fence { name: name.to_owned(), token }, lost))
    }
}
