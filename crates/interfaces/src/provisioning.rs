use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MachineName(pub String);

impl fmt::Display for MachineName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineState {
    Ready,
    Allocated,
    Deploying,
    Deployed,
    Failed { reason: String },
    Unavailable { status: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installation {
    pub user_data: String,
    pub is_in_memory: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProvisioningError {
    #[error("there is no machine named {0}")]
    UnknownMachine(MachineName),
    #[error("the machine provider refused the request: {0}")]
    Refused(String),
    #[error("the machine provider is unreachable: {0}")]
    Unreachable(String),
}

impl ProvisioningError {
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Unreachable(_))
    }
}

#[async_trait::async_trait]
pub trait MachineProvisioner: Send + Sync {
    async fn state(&self, machine: &MachineName) -> Result<MachineState, ProvisioningError>;

    async fn deploy(
        &self,
        machine: &MachineName,
        installation: &Installation,
    ) -> Result<(), ProvisioningError>;
}
