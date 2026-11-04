use std::fmt;

use amafu_api::ClientError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub description: String,
    pub details: Vec<(String, String)>,
}

impl Violation {
    pub fn new(description: impl Into<String>) -> Self {
        Self { description: description.into(), details: Vec::new() }
    }

    #[must_use]
    pub fn with(mut self, key: &str, value: impl fmt::Debug) -> Self {
        self.details.push((key.to_owned(), format!("{value:?}")));
        self
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.description)?;
        for (key, value) in &self.details {
            write!(formatter, " {key}={value}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScenarioError {
    #[error("invariant violated: {0}")]
    Violation(Violation),
    #[error("timed out waiting until {0}")]
    Timeout(String),
    #[error("expectation failed: {0}")]
    Expectation(String),
    #[error("API call failed: {0}")]
    Client(#[from] ClientError),
    #[error("the level cannot run this scenario: {0}")]
    Unsupported(String),
    #[error("the lab failed: {0}")]
    Lab(String),
}

impl From<Violation> for ScenarioError {
    fn from(violation: Violation) -> Self {
        Self::Violation(violation)
    }
}

pub type ScenarioResult = Result<(), ScenarioError>;

impl From<amafu_installer::certificates::IssuanceError> for ScenarioError {
    fn from(error: amafu_installer::certificates::IssuanceError) -> Self {
        Self::Lab(error.to_string())
    }
}
