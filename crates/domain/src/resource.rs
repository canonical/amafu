use std::fmt::Debug;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::validation::Validate;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    Cluster,
    Project,
}

pub trait Resource: Clone + Debug + Send + Sync + 'static {
    const KIND: &'static str;
    const PLURAL_NAME: &'static str;
    const VERSION: u32;
    const SCOPE: Scope;
    type Specification: Validate + Clone + Debug + PartialEq + Serialize + DeserializeOwned + Send + Sync;
    type Status: Clone + Debug + Default + PartialEq + Serialize + DeserializeOwned + Send + Sync;
}
