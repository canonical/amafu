use futures::future::LocalBoxFuture;

use crate::cluster::{Cluster, Level};
use crate::error::ScenarioResult;

pub type ScenarioFunction =
    for<'cluster> fn(&'cluster dyn Cluster) -> LocalBoxFuture<'cluster, ScenarioResult>;

#[derive(Clone, Copy)]
pub struct ScenarioDefinition {
    pub name: &'static str,
    pub levels: &'static [Level],
    pub node_count: usize,
    pub run: ScenarioFunction,
}

impl ScenarioDefinition {
    #[must_use]
    pub fn runs_at(&self, level: Level) -> bool {
        self.levels.contains(&level)
    }
}
