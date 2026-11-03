use amafu_domain::registry::{KindDescriptor, UntypedChange, UntypedChangeError};
use amafu_domain::{
    Change, ChangeError, Object, ObjectReference, ProjectName, Resource, apply_change, confirm_removal,
};
use serde_json::Value;

use crate::error::ActionError;
use crate::store::{Consistency, Fence, Store, StoreError, StoredRecord};

const MAXIMUM_ATTEMPTS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ObjectStoreError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Change(#[from] ChangeError),
    #[error("the request does not match the kind: {0}")]
    Malformed(String),
    #[error("a stored object cannot be decoded: {0}")]
    Corrupt(String),
    #[error("the object kept changing while being updated")]
    Contended,
}

impl From<UntypedChangeError> for ObjectStoreError {
    fn from(error: UntypedChangeError) -> Self {
        match error {
            UntypedChangeError::Malformed(reason) => Self::Malformed(reason),
            UntypedChangeError::Corrupt(reason) => Self::Corrupt(reason),
            UntypedChangeError::Change(change) => Self::Change(change),
        }
    }
}

impl From<ObjectStoreError> for ActionError {
    fn from(error: ObjectStoreError) -> Self {
        match &error {
            ObjectStoreError::Store(StoreError::Corrupt(_)) | ObjectStoreError::Corrupt(_) => {
                Self::fatal(error)
            }
            ObjectStoreError::Change(ChangeError::Invalid(_))
            | ObjectStoreError::Malformed(_)
            | ObjectStoreError::Store(StoreError::Refused(_)) => Self::invalid(error),
            ObjectStoreError::Store(StoreError::Fenced { .. }) => Self::fatal(error),
            ObjectStoreError::Store(_) | ObjectStoreError::Change(_) | ObjectStoreError::Contended => {
                Self::retryable(error)
            }
        }
    }
}

fn decode<R: Resource>(record: &StoredRecord) -> Result<Object<R>, ObjectStoreError> {
    serde_json::from_value(record.object.clone())
        .map_err(|error| ObjectStoreError::Corrupt(error.to_string()))
}

fn encode<R: Resource>(object: &Object<R>) -> Result<Value, ObjectStoreError> {
    serde_json::to_value(object).map_err(|error| ObjectStoreError::Corrupt(error.to_string()))
}

async fn update_with<F>(
    store: &dyn Store,
    reference: &ObjectReference,
    fence: Option<&Fence>,
    mut change: F,
) -> Result<Value, ObjectStoreError>
where
    F: FnMut(Option<&Value>) -> Result<Value, ObjectStoreError> + Send,
{
    for _ in 0..MAXIMUM_ATTEMPTS {
        let current = store.read(reference, Consistency::Linearizable).await?;
        let updated = change(current.as_ref().map(|record| &record.object))?;
        if let Some(record) = &current
            && record.object == updated
        {
            return Ok(updated);
        }
        let expected_revision = current.map(|record| record.revision);
        let swapped = match fence {
            Some(fence) => {
                store
                    .compare_and_swap_fenced(reference, expected_revision, Some(updated.clone()), fence)
                    .await
            }
            None => store.compare_and_swap(reference, expected_revision, Some(updated.clone())).await,
        };
        match swapped {
            Ok(_) => return Ok(updated),
            Err(StoreError::Conflict) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(ObjectStoreError::Contended)
}

async fn remove_with<F>(
    store: &dyn Store,
    reference: &ObjectReference,
    confirm: F,
) -> Result<(), ObjectStoreError>
where
    F: Fn(Option<&Value>) -> Result<(), ObjectStoreError> + Send + Sync,
{
    for _ in 0..MAXIMUM_ATTEMPTS {
        let current = store.read(reference, Consistency::Linearizable).await?;
        confirm(current.as_ref().map(|record| &record.object))?;
        let expected_revision = current.map(|record| record.revision);
        match store.compare_and_swap(reference, expected_revision, None).await {
            Ok(_) => return Ok(()),
            Err(StoreError::Conflict) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(ObjectStoreError::Contended)
}

pub async fn apply_untyped(
    store: &dyn Store,
    descriptor: &KindDescriptor,
    reference: &ObjectReference,
    change: UntypedChange,
) -> Result<Value, ObjectStoreError> {
    update_with(store, reference, None, |current| Ok((descriptor.apply_change)(current, change.clone())?))
        .await
}

pub async fn remove_untyped(
    store: &dyn Store,
    descriptor: &KindDescriptor,
    reference: &ObjectReference,
) -> Result<(), ObjectStoreError> {
    remove_with(store, reference, |current| Ok((descriptor.confirm_removal)(current)?)).await
}

pub async fn apply<R: Resource>(
    store: &dyn Store,
    reference: &ObjectReference,
    change: Change<R>,
) -> Result<Object<R>, ObjectStoreError> {
    apply_with_fence(store, reference, None, change).await
}

pub async fn apply_fenced<R: Resource>(
    store: &dyn Store,
    reference: &ObjectReference,
    fence: &Fence,
    change: Change<R>,
) -> Result<Object<R>, ObjectStoreError> {
    apply_with_fence(store, reference, Some(fence), change).await
}

async fn apply_with_fence<R: Resource>(
    store: &dyn Store,
    reference: &ObjectReference,
    fence: Option<&Fence>,
    change: Change<R>,
) -> Result<Object<R>, ObjectStoreError> {
    let updated = update_with(store, reference, fence, |current| {
        let current = current
            .map(|value| serde_json::from_value::<Object<R>>(value.clone()))
            .transpose()
            .map_err(|error| ObjectStoreError::Corrupt(error.to_string()))?;
        encode(&apply_change(current, change.clone())?)
    })
    .await?;
    serde_json::from_value(updated).map_err(|error| ObjectStoreError::Corrupt(error.to_string()))
}

pub async fn remove<R: Resource>(
    store: &dyn Store,
    reference: &ObjectReference,
) -> Result<(), ObjectStoreError> {
    remove_with(store, reference, |current| {
        let current = current
            .map(|value| serde_json::from_value::<Object<R>>(value.clone()))
            .transpose()
            .map_err(|error| ObjectStoreError::Corrupt(error.to_string()))?;
        Ok(confirm_removal(current.as_ref())?)
    })
    .await
}

pub async fn get<R: Resource>(
    store: &dyn Store,
    reference: &ObjectReference,
    consistency: Consistency,
) -> Result<Option<Object<R>>, ObjectStoreError> {
    store.read(reference, consistency).await?.as_ref().map(decode::<R>).transpose()
}

pub async fn list<R: Resource>(
    store: &dyn Store,
    project: Option<&ProjectName>,
    consistency: Consistency,
) -> Result<Vec<Object<R>>, ObjectStoreError> {
    store.list(R::KIND, project, consistency).await?.iter().map(decode::<R>).collect()
}
