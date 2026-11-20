use std::collections::BTreeMap;

use amafu_domain::{Change, Object, ObjectId, ObjectName, ObjectReference, ProjectName, Resource};
use amafu_interfaces::objects::{apply_fenced, get, list, remove, update_status_fenced};
use amafu_interfaces::{ActionError, Consistency, Fence, Identifiers, Store};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ownership {
    label: &'static str,
    environment: ObjectName,
    project: ProjectName,
}

impl Ownership {
    #[must_use]
    pub fn new(label: &'static str, environment: ObjectName, project: ProjectName) -> Self {
        Self { label, environment, project }
    }

    #[must_use]
    pub fn environment(&self) -> &ObjectName {
        &self.environment
    }

    #[must_use]
    pub fn project(&self) -> &ProjectName {
        &self.project
    }

    #[must_use]
    pub fn owns(&self, labels: &BTreeMap<String, String>) -> bool {
        labels.get(self.label) == Some(&self.environment.to_string())
    }

    #[must_use]
    pub fn labels(&self) -> BTreeMap<String, String> {
        BTreeMap::from([(self.label.to_owned(), self.environment.to_string())])
    }

    #[must_use]
    pub fn reference<R: Resource>(&self, name: ObjectName) -> ObjectReference {
        ObjectReference::of::<R>(Some(self.project.clone()), name)
    }

    pub fn named(&self, suffix: &str) -> Result<ObjectName, ActionError> {
        ObjectName::new(format!("{}-{suffix}", self.environment)).map_err(ActionError::fatal)
    }
}

#[derive(Clone, Copy)]
pub struct EnvironmentObjects<'a> {
    pub store: &'a dyn Store,
    pub fence: &'a Fence,
    pub identifiers: &'a dyn Identifiers,
}

impl EnvironmentObjects<'_> {
    pub async fn read<R: Resource>(
        &self,
        reference: &ObjectReference,
    ) -> Result<Option<Object<R>>, ActionError> {
        Ok(get::<R>(self.store, reference, Consistency::Linearizable).await?)
    }

    pub async fn require<R: Resource>(&self, reference: &ObjectReference) -> Result<Object<R>, ActionError> {
        self.read::<R>(reference).await?.ok_or_else(|| {
            ActionError::retryable(format!("{} {} does not exist yet", R::KIND, reference.name))
        })
    }

    pub async fn ensure<R: Resource>(
        &self,
        reference: &ObjectReference,
        labels: BTreeMap<String, String>,
        specification: R::Specification,
    ) -> Result<Object<R>, ActionError> {
        if let Some(found) = self.read::<R>(reference).await? {
            return Ok(found);
        }
        let change = Change::Create {
            id: ObjectId::from_uuid(self.identifiers.generate()),
            name: reference.name.clone(),
            project: reference.project.clone(),
            labels,
            specification,
        };
        Ok(apply_fenced::<R>(self.store, reference, self.fence, change).await?)
    }

    pub async fn change_specification<R: Resource>(
        &self,
        reference: &ObjectReference,
        change: impl FnOnce(&R::Specification) -> Option<R::Specification>,
    ) -> Result<(), ActionError> {
        let current = self.require::<R>(reference).await?;
        let Some(specification) = change(&current.specification) else {
            return Ok(());
        };
        let update = Change::UpdateSpecification {
            expected_generation: Some(current.metadata.generation),
            specification,
        };
        apply_fenced::<R>(self.store, reference, self.fence, update).await?;
        Ok(())
    }

    pub async fn change_status<R: Resource>(
        &self,
        reference: &ObjectReference,
        change: impl Fn(&Object<R>) -> Option<R::Status> + Send + Sync,
    ) -> Result<(), ActionError> {
        update_status_fenced::<R>(self.store, reference, self.fence, change).await?;
        Ok(())
    }

    pub async fn delete<R: Resource>(&self, reference: &ObjectReference) -> Result<bool, ActionError> {
        match self.read::<R>(reference).await? {
            None => Ok(true),
            Some(found) if found.metadata.is_deleting => Ok(false),
            Some(_) => {
                apply_fenced::<R>(self.store, reference, self.fence, Change::Delete).await?;
                Ok(false)
            }
        }
    }

    pub async fn delete_owned<R: Resource>(&self, ownership: &Ownership) -> Result<bool, ActionError> {
        let owned = list::<R>(self.store, Some(ownership.project()), Consistency::Linearizable).await?;
        let mut is_gone = true;
        for object in owned.iter().filter(|object| ownership.owns(&object.metadata.labels)) {
            is_gone &= self.delete::<R>(&object.reference()).await?;
        }
        Ok(is_gone)
    }

    pub async fn erase<R: Resource>(&self, reference: &ObjectReference) -> Result<(), ActionError> {
        match self.read::<R>(reference).await? {
            None => return Ok(()),
            Some(found) if !found.metadata.is_deleting => {
                apply_fenced::<R>(self.store, reference, self.fence, Change::Delete).await?;
            }
            Some(_) => {}
        }
        Ok(remove::<R>(self.store, reference).await?)
    }
}
