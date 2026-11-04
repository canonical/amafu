use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use amafu_domain::kinds::node::{BootReport, BootSlot, RefusedImage};
use amafu_domain::node_image::{ImageIdentity, NodeImage};
use amafu_interfaces::{BootSlotError, BootSlots, Clock, TrustedImageKeys};

use crate::lock::lock;

const WRITE_DURATION: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedImage {
    pub image: NodeImage,
    pub boots: bool,
}

#[derive(Default)]
pub struct ImageRepository {
    published: Mutex<BTreeMap<String, PublishedImage>>,
}

impl ImageRepository {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn publish(&self, image: NodeImage, boots: bool) {
        lock(&self.published).insert(image.url.clone(), PublishedImage { image, boots });
    }

    fn fetch(&self, url: &str) -> Option<PublishedImage> {
        lock(&self.published).get(url).cloned()
    }

    fn boots(&self, image: &NodeImage) -> bool {
        lock(&self.published)
            .values()
            .any(|published| published.image.is(&image.identity()) && published.boots)
    }
}

struct SlotState {
    first: Option<NodeImage>,
    second: Option<NodeImage>,
    default: BootSlot,
    booted: BootSlot,
    next: Option<BootSlot>,
    trial: Option<(BootSlot, ImageIdentity)>,
    fallen_back_from: Option<ImageIdentity>,
    refused: Option<RefusedImage>,
    wants_reboot: bool,
}

impl SlotState {
    fn image(&self, slot: BootSlot) -> Option<&NodeImage> {
        match slot {
            BootSlot::First => self.first.as_ref(),
            BootSlot::Second => self.second.as_ref(),
        }
    }

    fn set_image(&mut self, slot: BootSlot, image: NodeImage) {
        match slot {
            BootSlot::First => self.first = Some(image),
            BootSlot::Second => self.second = Some(image),
        }
    }
}

pub struct FakeBootSlots {
    clock: Arc<dyn Clock>,
    keys: TrustedImageKeys,
    repository: Arc<ImageRepository>,
    state: Mutex<SlotState>,
}

impl FakeBootSlots {
    #[must_use]
    pub fn installed(
        image: NodeImage,
        clock: Arc<dyn Clock>,
        keys: TrustedImageKeys,
        repository: Arc<ImageRepository>,
    ) -> Self {
        let state = SlotState {
            first: Some(image),
            second: None,
            default: BootSlot::First,
            booted: BootSlot::First,
            next: None,
            trial: None,
            fallen_back_from: None,
            refused: None,
            wants_reboot: false,
        };
        Self { clock, keys, repository, state: Mutex::new(state) }
    }

    #[must_use]
    pub fn wants_reboot(&self) -> bool {
        lock(&self.state).wants_reboot
    }

    pub fn power_on(&self) {
        let mut state = lock(&self.state);
        state.wants_reboot = false;
        let chosen = state.next.take().unwrap_or(state.default);
        let boots = state.image(chosen).is_some_and(|image| self.repository.boots(image));
        state.booted = if boots { chosen } else { state.default };
        if let Some((slot, image)) = state.trial.clone()
            && slot != state.booted
        {
            state.fallen_back_from = Some(image);
            state.trial = None;
        }
    }

    fn refuse(&self, image: &NodeImage, error: BootSlotError) -> BootSlotError {
        lock(&self.state).refused = Some(RefusedImage { image: image.identity(), reason: error.to_string() });
        error
    }
}

#[async_trait::async_trait]
impl BootSlots for FakeBootSlots {
    fn report(&self) -> BootReport {
        let state = lock(&self.state);
        BootReport {
            booted_slot: state.booted,
            default_slot: state.default,
            booted_image: state.image(state.booted).map(NodeImage::identity),
            inactive_image: state.image(state.booted.other()).map(NodeImage::identity),
            fallen_back_from: state.fallen_back_from.clone(),
            refused: state.refused.clone(),
        }
    }

    async fn write_inactive_slot(&self, image: &NodeImage) -> Result<(), BootSlotError> {
        if let Err(error) = self.keys.verify(image) {
            return Err(self.refuse(image, error.into()));
        }
        let published = self
            .repository
            .fetch(&image.url)
            .ok_or_else(|| BootSlotError::Fetch(format!("nothing is published at {}", image.url)))?;
        if published.image.digest != image.digest || published.image.size_bytes != image.size_bytes {
            return Err(
                self.refuse(image, BootSlotError::Mismatch(format!("{} serves other bytes", image.url)))
            );
        }
        self.clock.sleep(WRITE_DURATION).await;
        let mut state = lock(&self.state);
        let inactive = state.booted.other();
        state.set_image(inactive, image.clone());
        Ok(())
    }

    async fn boot_inactive_slot_next(&self) -> Result<(), BootSlotError> {
        let mut state = lock(&self.state);
        let inactive = state.booted.other();
        let image = state
            .image(inactive)
            .map(NodeImage::identity)
            .ok_or_else(|| BootSlotError::Write(String::from("the inactive slot is empty")))?;
        state.next = Some(inactive);
        state.trial = Some((inactive, image));
        Ok(())
    }

    async fn confirm_booted_slot(&self) -> Result<(), BootSlotError> {
        let mut state = lock(&self.state);
        state.default = state.booted;
        state.trial = None;
        Ok(())
    }

    async fn reboot(&self) -> Result<(), BootSlotError> {
        lock(&self.state).wants_reboot = true;
        Ok(())
    }
}
