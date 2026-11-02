use amafu_interfaces::StoreError;

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
