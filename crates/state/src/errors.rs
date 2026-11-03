use amafu_interfaces::{Fence, StoreError};

use crate::entities::FencedSwapError;

pub fn store_error<E>(error: quorium::Error<E>) -> StoreError {
    match error {
        quorium::Error::Rejected(_) => StoreError::Conflict,
        quorium::Error::Internal(message) => StoreError::Corrupt(message),
        quorium::Error::Unavailable => StoreError::Unavailable(String::from("no leader can be reached")),
        quorium::Error::Timeout => StoreError::Unavailable(String::from("the store did not answer in time")),
        quorium::Error::Shutdown => StoreError::Unavailable(String::from("the store is shutting down")),
        _ => StoreError::Unavailable(String::from("the store refused the request")),
    }
}

pub fn fenced_store_error(error: quorium::Error<FencedSwapError>, fence: &Fence) -> StoreError {
    match error {
        quorium::Error::Rejected(FencedSwapError::StaleToken) => {
            StoreError::Fenced { name: fence.name.clone(), token: fence.token }
        }
        other => store_error(other),
    }
}
