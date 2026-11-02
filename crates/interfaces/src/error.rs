use std::fmt::Display;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActionError {
    #[error("retryable: {0}")]
    Retryable(String),
    #[error("invalid: {0}")]
    Invalid(String),
    #[error("fatal: {0}")]
    Fatal(String),
}

impl ActionError {
    pub fn retryable(cause: impl Display) -> Self {
        Self::Retryable(cause.to_string())
    }

    pub fn invalid(cause: impl Display) -> Self {
        Self::Invalid(cause.to_string())
    }

    pub fn fatal(cause: impl Display) -> Self {
        Self::Fatal(cause.to_string())
    }

    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Retryable(_))
    }
}
