use std::time::Duration;

use amafu_domain::kinds::node_lease::LeaseTerms;
use amafu_interfaces::{Clock, HeldNodeLease, NodeLeaseAuthority, NodeLeaseError, NodeLeaseState, Watchdog};
use time::OffsetDateTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseKeeperSettings {
    pub terms: LeaseTerms,
    pub renewal_interval: Duration,
    pub retry_delay: Duration,
}

impl LeaseKeeperSettings {
    #[must_use]
    pub fn for_terms(terms: LeaseTerms) -> Self {
        Self { terms, renewal_interval: terms.duration() / 4, retry_delay: Duration::from_millis(500) }
    }
}

pub struct LeaseKeeper<'a> {
    pub authority: &'a dyn NodeLeaseAuthority,
    pub clock: &'a dyn Clock,
    pub state: &'a NodeLeaseState,
    pub settings: LeaseKeeperSettings,
}

impl LeaseKeeper<'_> {
    pub async fn keep(&self) -> ! {
        loop {
            let epoch = self.acquire().await;
            let lost = self.renew_until_refused(epoch).await;
            self.state.drop_lease();
            if self.state.is_surrendered() {
                tracing::info!(epoch, "the node surrendered its lease to reboot");
                std::future::pending::<()>().await;
            }
            tracing::error!(epoch, %lost, "the node lost its lease");
        }
    }

    fn valid_until(&self, sent_at: OffsetDateTime) -> OffsetDateTime {
        sent_at + self.settings.terms.duration()
    }

    async fn acquire(&self) -> u64 {
        loop {
            let sent_at = self.clock.now();
            match self.authority.acquire(self.settings.terms).await {
                Ok(grant) => {
                    self.state
                        .hold(HeldNodeLease { epoch: grant.epoch, valid_until: self.valid_until(sent_at) });
                    tracing::info!(epoch = grant.epoch, "the node holds its lease");
                    return grant.epoch;
                }
                Err(error) => {
                    tracing::warn!(%error, "the node cannot acquire its lease yet");
                    self.clock.sleep(self.settings.retry_delay).await;
                }
            }
        }
    }

    async fn renew_until_refused(&self, epoch: u64) -> NodeLeaseError {
        loop {
            self.clock.sleep(self.settings.renewal_interval).await;
            if self.state.is_surrendered() {
                return NodeLeaseError::Refused(amafu_domain::kinds::node_lease::LeaseRefusal::Lost);
            }
            let sent_at = self.clock.now();
            match self.authority.renew(epoch).await {
                Ok(grant) => self
                    .state
                    .hold(HeldNodeLease { epoch: grant.epoch, valid_until: self.valid_until(sent_at) }),
                Err(error) if error.is_refusal() => return error,
                Err(error) => tracing::warn!(%error, "renewing the node's lease failed; trying again"),
            }
        }
    }
}

pub async fn feed_while_leased(
    watchdog: &dyn Watchdog,
    state: &NodeLeaseState,
    clock: &dyn Clock,
    interval: Duration,
) -> ! {
    let mut is_armed = false;
    loop {
        if state.epoch_valid_at(clock.now()).is_some() {
            match watchdog.feed() {
                Ok(()) if !is_armed => {
                    is_armed = true;
                    tracing::info!(?interval, "the watchdog is armed");
                }
                Ok(()) => {}
                Err(error) => tracing::error!(%error, "feeding the watchdog failed"),
            }
        }
        clock.sleep(interval / 4).await;
    }
}
