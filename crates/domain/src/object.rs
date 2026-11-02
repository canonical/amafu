use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::name::{ObjectName, ProjectName};
use crate::object_id::ObjectId;
use crate::resource::{Resource, Scope};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
    pub name: ObjectName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectName>,
    pub id: ObjectId,
    pub version: u32,
    pub generation: u64,
    pub observed_generation: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub is_deleting: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ObjectReference {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectName>,
    pub name: ObjectName,
}

impl ObjectReference {
    #[must_use]
    pub fn of<R: Resource>(project: Option<ProjectName>, name: ObjectName) -> Self {
        let project = match R::SCOPE {
            Scope::Cluster => None,
            Scope::Project => project,
        };
        Self { kind: R::KIND.to_owned(), project, name }
    }
}

impl fmt::Display for ObjectReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.project {
            Some(project) => write!(formatter, "{}/{}/{}", self.kind, project, self.name),
            None => write!(formatter, "{}/{}", self.kind, self.name),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Object<R: Resource> {
    pub metadata: Metadata,
    pub specification: R::Specification,
    #[serde(default)]
    pub status: R::Status,
}

impl<R: Resource> Object<R> {
    #[must_use]
    pub fn reference(&self) -> ObjectReference {
        ObjectReference {
            kind: R::KIND.to_owned(),
            project: self.metadata.project.clone(),
            name: self.metadata.name.clone(),
        }
    }

    #[must_use]
    pub fn id(&self) -> ObjectId {
        self.metadata.id
    }

    #[must_use]
    pub fn name(&self) -> &ObjectName {
        &self.metadata.name
    }

    #[must_use]
    pub fn is_status_current(&self) -> bool {
        self.metadata.observed_generation == self.metadata.generation
    }
}
