use std::collections::BTreeMap;
use std::sync::Arc;

use amafu_domain::kinds::lxd_identity::{LxdIdentity, LxdIdentitySpecification, lxd_identity_name};
use amafu_domain::{Change, ObjectId, ObjectReference};
use amafu_interfaces::objects::{apply, get};
use amafu_interfaces::{ActionError, Clock, Consistency, Identifiers, Store};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};
use rcgen::{CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, KeyPair};
use time::{Duration, OffsetDateTime};

const CLIENT_NAME: &str = "amafu";
const VALIDITY: Duration = Duration::days(3650);

pub fn generate_identity(now: OffsetDateTime) -> Result<LxdIdentitySpecification, ActionError> {
    let key = KeyPair::generate().map_err(ActionError::fatal)?;
    let mut parameters = CertificateParams::new(Vec::<String>::new()).map_err(ActionError::fatal)?;
    let mut distinguished_name = DistinguishedName::new();
    distinguished_name.push(DnType::CommonName, CLIENT_NAME);
    parameters.distinguished_name = distinguished_name;
    parameters.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    parameters.not_before = now;
    parameters.not_after = now + VALIDITY;
    let certificate = parameters.self_signed(&key).map_err(ActionError::fatal)?;
    Ok(LxdIdentitySpecification { certificate_pem: certificate.pem(), private_key_pem: key.serialize_pem() })
}

fn reference() -> ObjectReference {
    ObjectReference::of::<LxdIdentity>(None, lxd_identity_name())
}

pub struct LxdIdentityController {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    identifiers: Arc<dyn Identifiers>,
}

impl LxdIdentityController {
    #[must_use]
    pub fn new(store: Arc<dyn Store>, clock: Arc<dyn Clock>, identifiers: Arc<dyn Identifiers>) -> Self {
        Self { store, clock, identifiers }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LxdIdentityObservation {
    pub exists: bool,
    pub now: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LxdIdentityAction {
    Create { now: OffsetDateTime },
}

impl Controller for LxdIdentityController {
    type Key = ();
    type Observation = LxdIdentityObservation;
    type Action = LxdIdentityAction;

    const NAME: &'static str = "lxd-identity";
    const WATCHES: &'static [WatchedKind<()>] =
        &[WatchedKind { kind: "LxdIdentity", mapping: KeyMapping::EveryKey }];

    async fn discover_keys(&self) -> Result<Vec<()>, ActionError> {
        Ok(vec![()])
    }

    async fn observe(&self, _key: &()) -> Result<LxdIdentityObservation, ActionError> {
        let found = get::<LxdIdentity>(self.store.as_ref(), &reference(), Consistency::Linearizable).await?;
        Ok(LxdIdentityObservation { exists: found.is_some(), now: self.clock.now() })
    }

    fn plan(&self, observation: &LxdIdentityObservation) -> Vec<LxdIdentityAction> {
        if observation.exists { Vec::new() } else { vec![LxdIdentityAction::Create { now: observation.now }] }
    }

    async fn apply(&self, _key: &(), action: LxdIdentityAction) -> Result<(), ActionError> {
        let LxdIdentityAction::Create { now } = action;
        let change = Change::Create {
            id: ObjectId::from_uuid(self.identifiers.generate()),
            name: lxd_identity_name(),
            project: None,
            labels: BTreeMap::new(),
            specification: generate_identity(now)?,
        };
        apply::<LxdIdentity>(self.store.as_ref(), &reference(), change).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use amafu_domain::Validate;

    use super::*;

    #[test]
    fn the_identity_is_a_valid_client_certificate_named_amafu() {
        let identity = generate_identity(OffsetDateTime::UNIX_EPOCH + Duration::days(20_000)).unwrap();
        assert_eq!(identity.validate(), Ok(()));
        let (_, pem) = x509_parser::pem::parse_x509_pem(identity.certificate_pem.as_bytes()).unwrap();
        let certificate = pem.parse_x509().unwrap();
        assert!(certificate.subject().to_string().contains("CN=amafu"));
        assert!(certificate.extended_key_usage().unwrap().unwrap().value.client_auth);
    }
}
