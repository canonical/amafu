use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use amafu_domain::NodeName;
use amafu_domain::kinds::node::{BootReport, Node, NodeImageRequest};
use amafu_domain::kinds::vm::{Vm, VmPhase};
use amafu_domain::node_image::NodeImage;
use amafu_interfaces::objects::{get, list};
use amafu_interfaces::{
    ActionError, BootSlots, Clock, Consistency, NodeLeaseAuthority, NodeLeaseState, Store,
};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};
use time::OffsetDateTime;

use crate::operations::node_specification::node_reference;

const UNCONFIRMED_REVISIT: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootSlotsSettings {
    pub confirm_after: Duration,
    pub give_up_after: Duration,
}

impl Default for BootSlotsSettings {
    fn default() -> Self {
        Self { confirm_after: Duration::from_secs(120), give_up_after: Duration::from_secs(600) }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BootSlotsObservation {
    pub report: BootReport,
    pub request: Option<NodeImageRequest>,
    pub healthy_for: Duration,
    pub running_for: Duration,
    pub hosts_running_vms: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootSlotsAction {
    WriteInactiveSlot(Box<NodeImage>),
    BootInactiveSlot,
    Confirm,
    GiveUp,
}

#[must_use]
pub fn plan_boot_slots(
    observation: &BootSlotsObservation,
    settings: &BootSlotsSettings,
) -> Vec<BootSlotsAction> {
    let report = &observation.report;
    if !report.is_confirmed() {
        if observation.healthy_for >= settings.confirm_after {
            return vec![BootSlotsAction::Confirm];
        }
        if observation.running_for >= settings.give_up_after {
            return vec![BootSlotsAction::GiveUp];
        }
        return Vec::new();
    }
    let Some(request) = &observation.request else {
        return Vec::new();
    };
    let image = &request.image;
    if report.runs(image) || report.fell_back_from(image) || report.refusal_of(image).is_some() {
        return Vec::new();
    }
    if !report.holds_in_inactive_slot(image) {
        return vec![BootSlotsAction::WriteInactiveSlot(Box::new(image.clone()))];
    }
    if request.should_boot && !observation.hosts_running_vms {
        return vec![BootSlotsAction::BootInactiveSlot];
    }
    Vec::new()
}

pub struct BootSlotsController {
    node: NodeName,
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    boot_slots: Arc<dyn BootSlots>,
    leases: Arc<dyn NodeLeaseAuthority>,
    lease: NodeLeaseState,
    settings: BootSlotsSettings,
    started_at: OffsetDateTime,
    healthy_since: Mutex<Option<OffsetDateTime>>,
}

pub struct BootSlotsServices {
    pub store: Arc<dyn Store>,
    pub clock: Arc<dyn Clock>,
    pub boot_slots: Arc<dyn BootSlots>,
    pub leases: Arc<dyn NodeLeaseAuthority>,
    pub lease: NodeLeaseState,
}

impl BootSlotsController {
    #[must_use]
    pub fn new(node: NodeName, services: BootSlotsServices, settings: BootSlotsSettings) -> Self {
        let started_at = services.clock.now();
        Self {
            node,
            store: services.store,
            clock: services.clock,
            boot_slots: services.boot_slots,
            leases: services.leases,
            lease: services.lease,
            settings,
            started_at,
            healthy_since: Mutex::new(None),
        }
    }

    fn healthy_for(&self, is_healthy: bool, now: OffsetDateTime) -> Duration {
        let mut healthy_since = self.healthy_since.lock().unwrap_or_else(PoisonError::into_inner);
        if !is_healthy {
            *healthy_since = None;
            return Duration::ZERO;
        }
        let since = *healthy_since.get_or_insert(now);
        Duration::try_from(now - since).unwrap_or_default()
    }

    async fn surrender_and_reboot(&self) -> Result<(), ActionError> {
        if let Some(held) = self.lease.surrender()
            && let Err(error) = self.leases.surrender(held.epoch).await
        {
            tracing::warn!(node = %self.node, %error, "the lease could not be surrendered before rebooting");
        }
        self.boot_slots.reboot().await.map_err(boot_slot_failure)
    }
}

fn boot_slot_failure(error: amafu_interfaces::BootSlotError) -> ActionError {
    if error.is_permanent() { ActionError::invalid(error) } else { ActionError::retryable(error) }
}

impl Controller for BootSlotsController {
    type Key = NodeName;
    type Observation = BootSlotsObservation;
    type Action = BootSlotsAction;

    const NAME: &'static str = "boot-slots";
    const WATCHES: &'static [WatchedKind<NodeName>] = &[
        WatchedKind {
            kind: "Node",
            mapping: KeyMapping::ObjectKey(|reference| NodeName::new(reference.name.as_str()).ok()),
        },
        WatchedKind { kind: "Vm", mapping: KeyMapping::EveryKey },
    ];

    async fn discover_keys(&self) -> Result<Vec<NodeName>, ActionError> {
        Ok(vec![self.node.clone()])
    }

    async fn observe(&self, _key: &NodeName) -> Result<BootSlotsObservation, ActionError> {
        let now = self.clock.now();
        let node =
            get::<Node>(self.store.as_ref(), &node_reference(&self.node), Consistency::Linearizable).await?;
        let is_healthy = self.lease.epoch_valid_at(now).is_some()
            && node.as_ref().is_some_and(|node| node.status.is_ready());
        let hosts_running_vms =
            list::<Vm>(self.store.as_ref(), None, Consistency::Linearizable).await?.iter().any(|vm| {
                vm.status.node.as_ref() == Some(&self.node)
                    && (vm.status.phase == VmPhase::Running || vm.status.is_migrating())
            });
        Ok(BootSlotsObservation {
            report: self.boot_slots.report(),
            request: node.and_then(|node| node.specification.image),
            healthy_for: self.healthy_for(is_healthy, now),
            running_for: Duration::try_from(now - self.started_at).unwrap_or_default(),
            hosts_running_vms,
        })
    }

    fn plan(&self, observation: &BootSlotsObservation) -> Vec<BootSlotsAction> {
        plan_boot_slots(observation, &self.settings)
    }

    async fn apply(&self, _key: &NodeName, action: BootSlotsAction) -> Result<(), ActionError> {
        match action {
            BootSlotsAction::WriteInactiveSlot(image) => {
                tracing::info!(node = %self.node, image = %image.identity(), "writing the inactive boot slot");
                self.boot_slots.write_inactive_slot(&image).await.map_err(boot_slot_failure)
            }
            BootSlotsAction::BootInactiveSlot => {
                self.boot_slots.boot_inactive_slot_next().await.map_err(boot_slot_failure)?;
                tracing::info!(node = %self.node, "rebooting into the inactive boot slot");
                self.surrender_and_reboot().await
            }
            BootSlotsAction::Confirm => {
                tracing::info!(node = %self.node, "the new boot slot stayed healthy, so it becomes the default");
                self.boot_slots.confirm_booted_slot().await.map_err(boot_slot_failure)
            }
            BootSlotsAction::GiveUp => {
                tracing::error!(node = %self.node, "the new boot slot never became healthy; going back to the default");
                self.surrender_and_reboot().await
            }
        }
    }

    fn revisit_after(&self, observation: &BootSlotsObservation) -> Option<Duration> {
        (!observation.report.is_confirmed()).then_some(UNCONFIRMED_REVISIT)
    }
}

#[cfg(test)]
mod tests {
    use amafu_domain::features::FeatureLevel;
    use amafu_domain::kinds::node::{BootSlot, RefusedImage};
    use amafu_domain::node_image::ImageIdentity;

    use super::*;

    fn image(version: &str) -> NodeImage {
        NodeImage {
            version: version.to_owned(),
            feature_level: FeatureLevel(1),
            digest: format!("{:0>64}", version.replace('.', "")).parse().unwrap(),
            size_bytes: 1024,
            url: format!("https://images.example/{version}.tar"),
            signature: "0".repeat(128),
        }
    }

    fn identity(version: &str) -> ImageIdentity {
        image(version).identity()
    }

    fn confirmed_on(version: &str) -> BootReport {
        BootReport {
            booted_slot: BootSlot::First,
            default_slot: BootSlot::First,
            booted_image: Some(identity(version)),
            inactive_image: None,
            fallen_back_from: None,
            refused: None,
        }
    }

    fn observation(report: BootReport, request: Option<NodeImageRequest>) -> BootSlotsObservation {
        BootSlotsObservation {
            report,
            request,
            healthy_for: Duration::ZERO,
            running_for: Duration::from_secs(5),
            hosts_running_vms: false,
        }
    }

    fn request(version: &str, should_boot: bool) -> NodeImageRequest {
        NodeImageRequest { image: image(version), should_boot }
    }

    #[test]
    fn a_requested_image_is_written_then_booted_only_when_asked_and_no_vm_runs_here() {
        let settings = BootSlotsSettings::default();
        let written = BootReport { inactive_image: Some(identity("0.2.0")), ..confirmed_on("0.1.0") };
        assert_eq!(
            plan_boot_slots(&observation(confirmed_on("0.1.0"), Some(request("0.2.0", false))), &settings),
            vec![BootSlotsAction::WriteInactiveSlot(Box::new(image("0.2.0")))]
        );
        assert_eq!(
            plan_boot_slots(&observation(written.clone(), Some(request("0.2.0", false))), &settings),
            Vec::new()
        );
        assert_eq!(
            plan_boot_slots(&observation(written.clone(), Some(request("0.2.0", true))), &settings),
            vec![BootSlotsAction::BootInactiveSlot]
        );
        let hosting = BootSlotsObservation {
            hosts_running_vms: true,
            ..observation(written, Some(request("0.2.0", true)))
        };
        assert_eq!(plan_boot_slots(&hosting, &settings), Vec::new());
    }

    #[test]
    fn a_trial_boot_is_confirmed_after_staying_healthy_and_abandoned_if_it_never_does() {
        let settings = BootSlotsSettings {
            confirm_after: Duration::from_secs(120),
            give_up_after: Duration::from_secs(600),
        };
        let trial = BootReport { booted_slot: BootSlot::Second, ..confirmed_on("0.2.0") };
        let young =
            BootSlotsObservation { healthy_for: Duration::from_secs(60), ..observation(trial.clone(), None) };
        assert_eq!(plan_boot_slots(&young, &settings), Vec::new());
        let healthy = BootSlotsObservation {
            healthy_for: Duration::from_secs(120),
            ..observation(trial.clone(), None)
        };
        assert_eq!(plan_boot_slots(&healthy, &settings), vec![BootSlotsAction::Confirm]);
        let stuck =
            BootSlotsObservation { running_for: Duration::from_secs(600), ..observation(trial, None) };
        assert_eq!(plan_boot_slots(&stuck, &settings), vec![BootSlotsAction::GiveUp]);
    }

    #[test]
    fn an_image_that_failed_or_was_refused_is_not_tried_again() {
        let settings = BootSlotsSettings::default();
        let fell_back = BootReport {
            fallen_back_from: Some(identity("0.2.0")),
            inactive_image: Some(identity("0.2.0")),
            ..confirmed_on("0.1.0")
        };
        assert_eq!(
            plan_boot_slots(&observation(fell_back, Some(request("0.2.0", true))), &settings),
            Vec::new()
        );
        let refused = BootReport {
            refused: Some(RefusedImage {
                image: image("0.2.0").identity(),
                reason: String::from("untrusted"),
            }),
            ..confirmed_on("0.1.0")
        };
        assert_eq!(
            plan_boot_slots(&observation(refused, Some(request("0.2.0", false))), &settings),
            Vec::new()
        );
        assert_eq!(
            plan_boot_slots(&observation(confirmed_on("0.2.0"), Some(request("0.2.0", true))), &settings),
            Vec::new()
        );
    }
}
