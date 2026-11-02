use std::fmt::Debug;
use std::future::Future;
use std::hash::Hash;
use std::time::Duration;

use amafu_domain::ObjectReference;
use amafu_interfaces::ActionError;

pub enum KeyMapping<Key> {
    ObjectKey(fn(&ObjectReference) -> Option<Key>),
    EveryKey,
}

pub struct WatchedKind<Key> {
    pub kind: &'static str,
    pub mapping: KeyMapping<Key>,
}

pub trait Controller: Send + Sync + 'static {
    type Key: Clone + Eq + Hash + Ord + Debug + Send + Sync + 'static;
    type Observation: Send;
    type Action: Debug + Send;

    const NAME: &'static str;
    const WATCHES: &'static [WatchedKind<Self::Key>];

    fn discover_keys(&self) -> impl Future<Output = Result<Vec<Self::Key>, ActionError>> + Send;

    fn observe(&self, key: &Self::Key)
    -> impl Future<Output = Result<Self::Observation, ActionError>> + Send;

    fn plan(&self, observation: &Self::Observation) -> Vec<Self::Action>;

    fn apply(
        &self,
        key: &Self::Key,
        action: Self::Action,
    ) -> impl Future<Output = Result<(), ActionError>> + Send;

    fn revisit_after(&self, _observation: &Self::Observation) -> Option<Duration> {
        None
    }
}
