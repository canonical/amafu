use std::time::Duration;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::access::ClusterPolicy;
use crate::name::NodeName;
use crate::resource::{Resource, Scope};
use crate::validation::{Validate, ValidationError};

pub const SHORTEST_TERM: Duration = Duration::from_secs(2);
pub const LONGEST_TERM: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeLease;

impl Resource for NodeLease {
    const KIND: &'static str = "NodeLease";
    const PLURAL_NAME: &'static str = "node-leases";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Cluster;
    const CLUSTER_POLICY: ClusterPolicy = ClusterPolicy::AdminOnly;
    type Specification = NodeLeaseSpecification;
    type Status = NodeLeaseStatus;
}

#[must_use]
pub fn node_fence_name(node: &NodeName) -> String {
    format!("node:{node}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseTerms {
    pub duration_milliseconds: u64,
    pub watchdog_interval_milliseconds: u64,
}

impl LeaseTerms {
    #[must_use]
    pub fn new(duration: Duration, watchdog_interval: Duration) -> Self {
        Self {
            duration_milliseconds: milliseconds(duration),
            watchdog_interval_milliseconds: milliseconds(watchdog_interval),
        }
    }

    #[must_use]
    pub fn duration(&self) -> Duration {
        Duration::from_millis(self.duration_milliseconds)
    }

    #[must_use]
    pub fn watchdog_interval(&self) -> Duration {
        Duration::from_millis(self.watchdog_interval_milliseconds)
    }

    #[must_use]
    pub fn is_within_bounds(&self) -> bool {
        let bounds = SHORTEST_TERM..=LONGEST_TERM;
        bounds.contains(&self.duration()) && bounds.contains(&self.watchdog_interval())
    }
}

fn milliseconds(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LeaseRefusal {
    #[error("the lease terms must be between 2 and 60 seconds")]
    TermsOutOfBounds,
    #[error("the node was lost and its VMs are not released yet")]
    NotReleased,
    #[error("the lease is held under epoch {current}, not {presented}")]
    EpochMismatch { current: u64, presented: u64 },
    #[error("the node was lost")]
    Lost,
    #[error("the node was lost already")]
    AlreadyLost,
    #[error("the lease has not expired")]
    NotExpired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeLeaseSpecification {
    pub epoch: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub renewed_at: OffsetDateTime,
    pub terms: LeaseTerms,
    #[serde(default)]
    pub is_lost: bool,
    #[serde(default)]
    pub is_released: bool,
}

impl NodeLeaseSpecification {
    pub fn acquired(
        existing: Option<&Self>,
        terms: LeaseTerms,
        now: OffsetDateTime,
    ) -> Result<Self, LeaseRefusal> {
        if !terms.is_within_bounds() {
            return Err(LeaseRefusal::TermsOutOfBounds);
        }
        let epoch = match existing {
            None => 1,
            Some(lease) if lease.is_lost && !lease.is_released => return Err(LeaseRefusal::NotReleased),
            Some(lease) => lease.epoch + 1,
        };
        Ok(Self { epoch, renewed_at: now, terms, is_lost: false, is_released: false })
    }

    pub fn renewed(&self, presented_epoch: u64, now: OffsetDateTime) -> Result<Self, LeaseRefusal> {
        if self.is_lost {
            return Err(LeaseRefusal::Lost);
        }
        if self.epoch != presented_epoch {
            return Err(LeaseRefusal::EpochMismatch { current: self.epoch, presented: presented_epoch });
        }
        Ok(Self { renewed_at: now.max(self.renewed_at), ..self.clone() })
    }

    pub fn marked_lost(&self, now: OffsetDateTime) -> Result<Self, LeaseRefusal> {
        if self.is_lost {
            return Err(LeaseRefusal::AlreadyLost);
        }
        if !self.has_expired(now) {
            return Err(LeaseRefusal::NotExpired);
        }
        Ok(Self { epoch: self.epoch + 1, is_lost: true, is_released: false, ..self.clone() })
    }

    pub fn surrendered(&self, presented_epoch: u64) -> Result<Self, LeaseRefusal> {
        self.admits(presented_epoch)?;
        Ok(Self { epoch: self.epoch + 1, is_lost: true, is_released: true, ..self.clone() })
    }

    #[must_use]
    pub fn released(&self) -> Self {
        Self { is_released: true, ..self.clone() }
    }

    pub fn admits(&self, presented_epoch: u64) -> Result<(), LeaseRefusal> {
        if self.is_lost {
            return Err(LeaseRefusal::Lost);
        }
        if self.epoch != presented_epoch {
            return Err(LeaseRefusal::EpochMismatch { current: self.epoch, presented: presented_epoch });
        }
        Ok(())
    }

    #[must_use]
    pub fn expires_at(&self) -> OffsetDateTime {
        self.renewed_at + self.terms.duration()
    }

    #[must_use]
    pub fn watchdog_deadline(&self) -> OffsetDateTime {
        self.expires_at() + self.terms.watchdog_interval()
    }

    #[must_use]
    pub fn has_expired(&self, now: OffsetDateTime) -> bool {
        now >= self.expires_at()
    }
}

impl Validate for NodeLeaseSpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(self.epoch >= 1, "specification.epoch", "must be at least 1");
        error.check(self.terms.is_within_bounds(), "specification.terms", "must be between 2 and 60 seconds");
        error.check(
            !self.is_released || self.is_lost,
            "specification.is_released",
            "applies to lost nodes only",
        );
        error.into_result()
    }

    fn validate_update(&self, previous: &Self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(self.epoch >= previous.epoch, "specification.epoch", "cannot go back");
        error.into_result()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NodeLeaseStatus {}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms() -> LeaseTerms {
        LeaseTerms::new(Duration::from_secs(8), Duration::from_secs(8))
    }

    fn at(seconds: i64) -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(seconds)
    }

    #[test]
    fn every_acquisition_starts_a_new_epoch() {
        let first = NodeLeaseSpecification::acquired(None, terms(), at(0)).unwrap();
        assert_eq!(first.epoch, 1);
        let second = NodeLeaseSpecification::acquired(Some(&first), terms(), at(1)).unwrap();
        assert_eq!(second.epoch, 2);
        assert_eq!(second.validate_update(&first), Ok(()));
        assert!(first.validate_update(&second).is_err());
    }

    #[test]
    fn a_lost_node_gets_a_lease_only_after_its_vms_are_released() {
        let held = NodeLeaseSpecification::acquired(None, terms(), at(0)).unwrap();
        assert_eq!(held.marked_lost(at(7)), Err(LeaseRefusal::NotExpired));
        let lost = held.marked_lost(at(8)).unwrap();
        assert_eq!(lost.epoch, 2);
        assert_eq!(
            NodeLeaseSpecification::acquired(Some(&lost), terms(), at(9)),
            Err(LeaseRefusal::NotReleased)
        );
        assert_eq!(lost.renewed(1, at(9)), Err(LeaseRefusal::Lost));
        assert_eq!(lost.admits(2), Err(LeaseRefusal::Lost));
        let returned = NodeLeaseSpecification::acquired(Some(&lost.released()), terms(), at(30)).unwrap();
        assert_eq!(returned.epoch, 3);
        assert!(!returned.is_lost);
    }

    #[test]
    fn a_surrendered_lease_fences_the_old_epoch_and_can_be_taken_again_at_once() {
        let held = NodeLeaseSpecification::acquired(None, terms(), at(0)).unwrap();
        assert!(held.surrendered(2).is_err());
        let surrendered = held.surrendered(1).unwrap();
        assert_eq!(surrendered.validate(), Ok(()));
        assert_eq!(surrendered.admits(1), Err(LeaseRefusal::Lost));
        assert_eq!(surrendered.renewed(1, at(1)), Err(LeaseRefusal::Lost));
        let returned = NodeLeaseSpecification::acquired(Some(&surrendered), terms(), at(1)).unwrap();
        assert_eq!(returned.epoch, 3);
    }

    #[test]
    fn only_the_current_epoch_renews_and_writes() {
        let held = NodeLeaseSpecification::acquired(None, terms(), at(0)).unwrap();
        assert_eq!(held.renewed(1, at(5)).unwrap().expires_at(), at(13));
        assert_eq!(held.renewed(7, at(5)), Err(LeaseRefusal::EpochMismatch { current: 1, presented: 7 }));
        assert_eq!(held.admits(1), Ok(()));
        assert!(held.admits(2).is_err());
    }

    #[test]
    fn the_watchdog_deadline_follows_the_expiry() {
        let held = NodeLeaseSpecification::acquired(None, terms(), at(0)).unwrap();
        assert_eq!(held.watchdog_deadline(), at(16));
    }

    #[test]
    fn terms_outside_the_bounds_are_refused() {
        let short = LeaseTerms::new(Duration::from_millis(500), Duration::from_secs(8));
        assert_eq!(NodeLeaseSpecification::acquired(None, short, at(0)), Err(LeaseRefusal::TermsOutOfBounds));
    }
}
