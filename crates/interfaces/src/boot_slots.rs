use amafu_domain::kinds::node::BootReport;
use amafu_domain::node_image::NodeImage;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BootSlotError {
    #[error("the image could not be fetched: {0}")]
    Fetch(String),
    #[error("the image is not the one that was signed: {0}")]
    Mismatch(String),
    #[error(transparent)]
    Untrusted(#[from] crate::node_image::ImageTrustError),
    #[error("the boot partition could not be written: {0}")]
    Write(String),
    #[error("the node could not be rebooted: {0}")]
    Reboot(String),
}

impl BootSlotError {
    #[must_use]
    pub fn is_permanent(&self) -> bool {
        matches!(self, Self::Mismatch(_) | Self::Untrusted(_))
    }
}

#[async_trait::async_trait]
pub trait BootSlots: Send + Sync {
    fn report(&self) -> BootReport;

    async fn write_inactive_slot(&self, image: &NodeImage) -> Result<(), BootSlotError>;

    async fn boot_inactive_slot_next(&self) -> Result<(), BootSlotError>;

    async fn confirm_booted_slot(&self) -> Result<(), BootSlotError>;

    async fn reboot(&self) -> Result<(), BootSlotError>;
}
