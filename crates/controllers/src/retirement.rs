use std::sync::Arc;

use amafu_domain::kinds::certificate_revocation::CertificateRevocation;
use amafu_domain::kinds::external_network::ExternalNetwork;
use amafu_domain::kinds::floating_ip::FloatingIp;
use amafu_domain::{Object, ObjectReference};
use amafu_interfaces::objects::{get, list, remove};
use amafu_interfaces::{ActionError, Consistency, Store};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetirementObservation {
    pub is_deleting: bool,
    pub is_in_use: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetirementAction {
    Remove,
}

#[must_use]
pub fn plan_retirement(observation: &RetirementObservation) -> Vec<RetirementAction> {
    if observation.is_deleting && !observation.is_in_use {
        vec![RetirementAction::Remove]
    } else {
        Vec::new()
    }
}

pub struct ExternalNetworkController {
    store: Arc<dyn Store>,
}

impl ExternalNetworkController {
    #[must_use]
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }
}

impl Controller for ExternalNetworkController {
    type Key = ObjectReference;
    type Observation = RetirementObservation;
    type Action = RetirementAction;

    const NAME: &'static str = "external-network";
    const WATCHES: &'static [WatchedKind<ObjectReference>] = &[
        WatchedKind {
            kind: "ExternalNetwork",
            mapping: KeyMapping::ObjectKey(|reference| Some(reference.clone())),
        },
        WatchedKind { kind: "FloatingIp", mapping: KeyMapping::EveryKey },
    ];

    async fn discover_keys(&self) -> Result<Vec<ObjectReference>, ActionError> {
        let networks = list::<ExternalNetwork>(self.store.as_ref(), None, Consistency::Linearizable).await?;
        Ok(networks.iter().map(Object::reference).collect())
    }

    async fn observe(&self, key: &ObjectReference) -> Result<RetirementObservation, ActionError> {
        let store = self.store.as_ref();
        let Some(network) = get::<ExternalNetwork>(store, key, Consistency::Linearizable).await? else {
            return Ok(RetirementObservation { is_deleting: false, is_in_use: false });
        };
        let is_in_use = list::<FloatingIp>(store, None, Consistency::Linearizable)
            .await?
            .iter()
            .any(|floating_ip| floating_ip.specification.external_network == network.metadata.name);
        Ok(RetirementObservation { is_deleting: network.metadata.is_deleting, is_in_use })
    }

    fn plan(&self, observation: &RetirementObservation) -> Vec<RetirementAction> {
        plan_retirement(observation)
    }

    async fn apply(&self, key: &ObjectReference, action: RetirementAction) -> Result<(), ActionError> {
        let RetirementAction::Remove = action;
        Ok(remove::<ExternalNetwork>(self.store.as_ref(), key).await?)
    }
}

pub struct CertificateRevocationController {
    store: Arc<dyn Store>,
}

impl CertificateRevocationController {
    #[must_use]
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }
}

impl Controller for CertificateRevocationController {
    type Key = ObjectReference;
    type Observation = RetirementObservation;
    type Action = RetirementAction;

    const NAME: &'static str = "certificate-revocation";
    const WATCHES: &'static [WatchedKind<ObjectReference>] = &[WatchedKind {
        kind: "CertificateRevocation",
        mapping: KeyMapping::ObjectKey(|reference| Some(reference.clone())),
    }];

    async fn discover_keys(&self) -> Result<Vec<ObjectReference>, ActionError> {
        let revocations =
            list::<CertificateRevocation>(self.store.as_ref(), None, Consistency::Linearizable).await?;
        Ok(revocations.iter().map(Object::reference).collect())
    }

    async fn observe(&self, key: &ObjectReference) -> Result<RetirementObservation, ActionError> {
        let revocation =
            get::<CertificateRevocation>(self.store.as_ref(), key, Consistency::Linearizable).await?;
        Ok(RetirementObservation {
            is_deleting: revocation.is_some_and(|revocation| revocation.metadata.is_deleting),
            is_in_use: false,
        })
    }

    fn plan(&self, observation: &RetirementObservation) -> Vec<RetirementAction> {
        plan_retirement(observation)
    }

    async fn apply(&self, key: &ObjectReference, action: RetirementAction) -> Result<(), ActionError> {
        let RetirementAction::Remove = action;
        Ok(remove::<CertificateRevocation>(self.store.as_ref(), key).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_deleted_object_is_removed_once_nothing_uses_it() {
        assert_eq!(
            plan_retirement(&RetirementObservation { is_deleting: true, is_in_use: false }),
            vec![RetirementAction::Remove]
        );
        assert_eq!(
            plan_retirement(&RetirementObservation { is_deleting: true, is_in_use: true }),
            Vec::new()
        );
        assert_eq!(
            plan_retirement(&RetirementObservation { is_deleting: false, is_in_use: false }),
            Vec::new()
        );
    }
}
