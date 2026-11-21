use std::sync::Arc;

use amafu_domain::kinds::certificate_revocation::CertificateRevocation;
use amafu_domain::kinds::external_network::ExternalNetwork;
use amafu_domain::kinds::floating_ip::FloatingIp;
use amafu_domain::kinds::ssh_key::{SshKey, SshKeyGroup};
use amafu_domain::{Object, ObjectReference, Resource};
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

pub struct SshKeyRetirementController {
    store: Arc<dyn Store>,
}

impl SshKeyRetirementController {
    #[must_use]
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }

    async fn references<R: Resource>(&self) -> Result<Vec<ObjectReference>, ActionError> {
        let objects = list::<R>(self.store.as_ref(), None, Consistency::Linearizable).await?;
        Ok(objects.iter().map(Object::reference).collect())
    }

    async fn is_deleting<R: Resource>(&self, key: &ObjectReference) -> Result<bool, ActionError> {
        let object = get::<R>(self.store.as_ref(), key, Consistency::Linearizable).await?;
        Ok(object.is_some_and(|object| object.metadata.is_deleting))
    }
}

impl Controller for SshKeyRetirementController {
    type Key = ObjectReference;
    type Observation = RetirementObservation;
    type Action = RetirementAction;

    const NAME: &'static str = "ssh-key-retirement";
    const WATCHES: &'static [WatchedKind<ObjectReference>] = &[
        WatchedKind { kind: "SshKey", mapping: KeyMapping::ObjectKey(|reference| Some(reference.clone())) },
        WatchedKind {
            kind: "SshKeyGroup",
            mapping: KeyMapping::ObjectKey(|reference| Some(reference.clone())),
        },
    ];

    async fn discover_keys(&self) -> Result<Vec<ObjectReference>, ActionError> {
        let mut keys = self.references::<SshKey>().await?;
        keys.extend(self.references::<SshKeyGroup>().await?);
        Ok(keys)
    }

    async fn observe(&self, key: &ObjectReference) -> Result<RetirementObservation, ActionError> {
        let is_deleting = if key.kind == SshKeyGroup::KIND {
            self.is_deleting::<SshKeyGroup>(key).await?
        } else {
            self.is_deleting::<SshKey>(key).await?
        };
        Ok(RetirementObservation { is_deleting, is_in_use: false })
    }

    fn plan(&self, observation: &RetirementObservation) -> Vec<RetirementAction> {
        plan_retirement(observation)
    }

    async fn apply(&self, key: &ObjectReference, action: RetirementAction) -> Result<(), ActionError> {
        let RetirementAction::Remove = action;
        let store = self.store.as_ref();
        if key.kind == SshKeyGroup::KIND {
            Ok(remove::<SshKeyGroup>(store, key).await?)
        } else {
            Ok(remove::<SshKey>(store, key).await?)
        }
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
