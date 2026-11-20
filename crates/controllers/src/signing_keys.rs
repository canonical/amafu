use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use amafu_domain::kinds::signing_key::{PUBLISHED_AHEAD, SigningKey, SigningKeySpecification};
use amafu_domain::{Change, ObjectId, ObjectName, ObjectReference};
use amafu_interfaces::objects::{apply, list, remove};
use amafu_interfaces::{ActionError, Clock, Consistency, Identifiers, Store};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};
use amafu_token_issuer::KeyPair;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use time::OffsetDateTime;

const REVISIT: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SigningKeysObservation {
    pub keys: Vec<(ObjectName, SigningKeySpecification)>,
    pub now: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SigningKeyAction {
    Create { signs_from: OffsetDateTime },
    Remove { name: ObjectName },
}

#[must_use]
pub fn plan_signing_keys(observation: &SigningKeysObservation) -> Vec<SigningKeyAction> {
    let now = observation.now;
    let keys = || observation.keys.iter().map(|(_, key)| key);
    let mut actions: Vec<SigningKeyAction> = observation
        .keys
        .iter()
        .filter(|(_, key)| !key.is_published_at(now) && key.signs_until <= now)
        .map(|(name, _)| SigningKeyAction::Remove { name: name.clone() })
        .collect();
    if !keys().any(|key| key.signs_at(now + PUBLISHED_AHEAD)) {
        let latest_end = keys().map(|key| key.signs_until).filter(|end| *end > now).max();
        actions.push(SigningKeyAction::Create { signs_from: latest_end.unwrap_or(now) });
    }
    actions
}

pub struct SigningKeysController {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    identifiers: Arc<dyn Identifiers>,
}

impl SigningKeysController {
    #[must_use]
    pub fn new(store: Arc<dyn Store>, clock: Arc<dyn Clock>, identifiers: Arc<dyn Identifiers>) -> Self {
        Self { store, clock, identifiers }
    }

    async fn create(&self, signs_from: OffsetDateTime) -> Result<(), ActionError> {
        let key = KeyPair::generate().map_err(|error| ActionError::retryable(error.to_string()))?;
        let name =
            ObjectName::new(format!("key-{}", signs_from.unix_timestamp())).map_err(ActionError::fatal)?;
        let specification =
            SigningKeySpecification::window_starting(STANDARD.encode(key.pkcs8()), signs_from);
        apply::<SigningKey>(
            self.store.as_ref(),
            &ObjectReference::of::<SigningKey>(None, name.clone()),
            Change::Create {
                id: ObjectId::from_uuid(self.identifiers.generate()),
                name,
                project: None,
                labels: BTreeMap::new(),
                specification,
            },
        )
        .await?;
        Ok(())
    }
}

impl Controller for SigningKeysController {
    type Key = ();
    type Observation = SigningKeysObservation;
    type Action = SigningKeyAction;

    const NAME: &'static str = "signing-keys";
    const WATCHES: &'static [WatchedKind<()>] =
        &[WatchedKind { kind: "SigningKey", mapping: KeyMapping::EveryKey }];

    async fn discover_keys(&self) -> Result<Vec<()>, ActionError> {
        Ok(vec![()])
    }

    async fn observe(&self, _key: &()) -> Result<SigningKeysObservation, ActionError> {
        let keys = list::<SigningKey>(self.store.as_ref(), None, Consistency::Linearizable)
            .await?
            .into_iter()
            .map(|key| (key.metadata.name, key.specification))
            .collect();
        Ok(SigningKeysObservation { keys, now: self.clock.now() })
    }

    fn plan(&self, observation: &SigningKeysObservation) -> Vec<SigningKeyAction> {
        plan_signing_keys(observation)
    }

    async fn apply(&self, _key: &(), action: SigningKeyAction) -> Result<(), ActionError> {
        match action {
            SigningKeyAction::Create { signs_from } => self.create(signs_from).await,
            SigningKeyAction::Remove { name } => {
                Ok(remove::<SigningKey>(self.store.as_ref(), &ObjectReference::of::<SigningKey>(None, name))
                    .await?)
            }
        }
    }

    fn revisit_after(&self, _observation: &SigningKeysObservation) -> Option<Duration> {
        Some(REVISIT)
    }
}

#[cfg(test)]
mod tests {
    use time::Duration as Span;

    use super::*;

    fn day(number: i64) -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Span::days(number)
    }

    fn key(starting_day: i64) -> (ObjectName, SigningKeySpecification) {
        (
            ObjectName::new(format!("key-{starting_day}")).unwrap(),
            SigningKeySpecification::window_starting(String::from("pkcs8"), day(starting_day)),
        )
    }

    #[test]
    fn a_cluster_without_keys_gets_one_that_signs_now() {
        let actions = plan_signing_keys(&SigningKeysObservation { keys: vec![], now: day(5) });
        assert_eq!(actions, vec![SigningKeyAction::Create { signs_from: day(5) }]);
    }

    #[test]
    fn the_next_key_starts_where_the_current_one_ends_a_day_ahead() {
        let keys = vec![key(0)];
        assert_eq!(plan_signing_keys(&SigningKeysObservation { keys: keys.clone(), now: day(20) }), vec![]);
        assert_eq!(
            plan_signing_keys(&SigningKeysObservation { keys, now: day(29) + Span::hours(1) }),
            vec![SigningKeyAction::Create { signs_from: day(30) }]
        );
    }

    #[test]
    fn a_key_is_removed_once_it_neither_signs_nor_is_published() {
        let keys = vec![key(0), key(30)];
        assert_eq!(
            plan_signing_keys(&SigningKeysObservation { keys: keys.clone(), now: day(30) + Span::hours(12) }),
            vec![]
        );
        assert_eq!(
            plan_signing_keys(&SigningKeysObservation { keys, now: day(32) }),
            vec![SigningKeyAction::Remove { name: key(0).0 }]
        );
    }
}
