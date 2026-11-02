pub mod hypervisor;
pub mod store;

use std::fmt::Debug;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("contract violated: {expectation}: {observed}")]
pub struct ContractViolation {
    pub expectation: &'static str,
    pub observed: String,
}

fn ensure(is_held: bool, expectation: &'static str, observed: impl Debug) -> Result<(), ContractViolation> {
    if is_held { Ok(()) } else { Err(ContractViolation { expectation, observed: format!("{observed:?}") }) }
}

fn succeed<T, E: Debug>(result: Result<T, E>, expectation: &'static str) -> Result<T, ContractViolation> {
    result.map_err(|error| ContractViolation { expectation, observed: format!("{error:?}") })
}
