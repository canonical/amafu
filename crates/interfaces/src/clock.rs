use std::time::Duration;

use futures::future::BoxFuture;
use time::OffsetDateTime;

pub trait Clock: Send + Sync {
    fn now(&self) -> OffsetDateTime;

    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()>;
}
