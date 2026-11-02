use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Violation {
    pub field: String,
    pub problem: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, thiserror::Error)]
pub struct ValidationError {
    pub violations: Vec<Violation>,
}

impl ValidationError {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, field: impl Into<String>, problem: impl Into<String>) {
        self.violations.push(Violation { field: field.into(), problem: problem.into() });
    }

    pub fn check(&mut self, is_valid: bool, field: &str, problem: &str) {
        if !is_valid {
            self.add(field, problem);
        }
    }

    pub fn into_result(self) -> Result<(), Self> {
        if self.violations.is_empty() { Ok(()) } else { Err(self) }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let described: Vec<String> = self
            .violations
            .iter()
            .map(|violation| format!("{}: {}", violation.field, violation.problem))
            .collect();
        write!(formatter, "invalid specification: {}", described.join("; "))
    }
}

pub trait Validate {
    fn validate(&self) -> Result<(), ValidationError>;
}
