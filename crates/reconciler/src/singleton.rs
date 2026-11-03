use std::pin::pin;

use amafu_interfaces::{Clock, Fence, Leases, Store};
use futures::future::{self, Either};

use crate::controller::Controller;
use crate::run::{ControllerFailure, run_controller};
use crate::settings::ReconcilerSettings;

pub async fn run_singleton<C: Controller>(
    leases: &dyn Leases,
    make: impl Fn(Fence) -> C,
    store: &dyn Store,
    clock: &dyn Clock,
    settings: ReconcilerSettings,
) -> ControllerFailure {
    loop {
        let lease = match leases.acquire(C::NAME).await {
            Ok(lease) => lease,
            Err(error) => {
                return ControllerFailure {
                    controller: C::NAME,
                    key: String::from("lease"),
                    cause: error.to_string(),
                };
            }
        };
        let (fence, lost) = lease.into_parts();
        tracing::info!(controller = C::NAME, token = fence.token, "holding the lease");
        let controller = make(fence);
        match future::select(pin!(run_controller(&controller, store, clock, settings)), lost).await {
            Either::Left((failure, _)) => return failure,
            Either::Right(((), _)) => {
                tracing::warn!(controller = C::NAME, "lost the lease; waiting to hold it again")
            }
        }
    }
}
