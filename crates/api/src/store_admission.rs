use std::collections::BTreeMap;

use amafu_domain::kinds::environment::{EnvironmentChange, EnvironmentPhase};
use amafu_domain::kinds::juju_controller::{
    JujuController, JujuControllerChange, JujuControllerChangeSpecification, JujuRequest,
};
use amafu_domain::kinds::kubernetes_cluster::{
    KubernetesCluster, KubernetesClusterChange, KubernetesClusterChangeSpecification, KubernetesRequest,
};
use amafu_domain::kinds::operation::OperationStatus;
use amafu_domain::kinds::vm::Vm;
use amafu_domain::labels::owning_environment;
use amafu_domain::{ObjectName, ObjectReference, ProjectName, Resource};
use amafu_interfaces::objects::{get, list};
use amafu_interfaces::{Consistency, ObjectStoreError, Store};
use serde_json::Value;

#[derive(Debug)]
pub enum Refusal {
    InUse(String),
    Owned(String),
    Unready(String),
    Store(ObjectStoreError),
}

impl From<ObjectStoreError> for Refusal {
    fn from(error: ObjectStoreError) -> Self {
        Self::Store(error)
    }
}

pub struct StoreAdmission<'a> {
    pub store: &'a dyn Store,
}

impl StoreAdmission<'_> {
    pub async fn on_create(
        &self,
        reference: &ObjectReference,
        labels: &BTreeMap<String, String>,
        specification: &Value,
    ) -> Result<(), Refusal> {
        refuse_environment_labels(labels)?;
        match reference.kind.as_str() {
            kind if kind == KubernetesClusterChange::KIND => {
                let change = decode::<KubernetesClusterChangeSpecification>(specification)?;
                self.admit_kubernetes_change(reference.project.as_ref(), &change).await?;
                self.admit_in_order::<KubernetesClusterChange, KubernetesCluster>(
                    reference.project.as_ref(),
                    &change,
                )
                .await
            }
            kind if kind == JujuControllerChange::KIND => {
                let change = decode::<JujuControllerChangeSpecification>(specification)?;
                self.admit_juju_change(reference.project.as_ref(), &change).await?;
                self.admit_in_order::<JujuControllerChange, JujuController>(
                    reference.project.as_ref(),
                    &change,
                )
                .await
            }
            _ => Ok(()),
        }
    }

    pub async fn on_update(&self, reference: &ObjectReference) -> Result<(), Refusal> {
        self.refuse_if_owned(reference).await
    }

    pub async fn on_delete(&self, reference: &ObjectReference) -> Result<(), Refusal> {
        self.refuse_if_owned(reference).await?;
        let holders = match reference.kind.as_str() {
            "Volume" => self.volume_holders(reference).await?,
            _ => Vec::new(),
        };
        if holders.is_empty() {
            return Ok(());
        }
        Err(Refusal::InUse(format!("{} is in use by {}", reference.name, holders.join(", "))))
    }

    async fn refuse_if_owned(&self, reference: &ObjectReference) -> Result<(), Refusal> {
        let Some(record) =
            self.store.read(reference, Consistency::Linearizable).await.map_err(ObjectStoreError::from)?
        else {
            return Ok(());
        };
        let labels: BTreeMap<String, String> =
            serde_json::from_value(record.object["metadata"]["labels"].clone()).unwrap_or_default();
        refuse_environment_labels(&labels)
    }

    async fn volume_holders(&self, volume: &ObjectReference) -> Result<Vec<String>, Refusal> {
        let vms = list::<Vm>(self.store, volume.project.as_ref(), Consistency::Linearizable).await?;
        Ok(vms
            .iter()
            .filter(|vm| vm.specification.volumes.contains(&volume.name))
            .map(|vm| format!("VM {}", vm.metadata.name))
            .collect())
    }

    async fn admit_in_order<Change, Environment>(
        &self,
        project: Option<&ProjectName>,
        change: &Change::Specification,
    ) -> Result<(), Refusal>
    where
        Change: Resource<Status = OperationStatus, Specification: EnvironmentChange>,
        Environment: Resource,
    {
        let environment = change.environment();
        let changes = list::<Change>(self.store, project, Consistency::Linearizable).await?;
        if let Some(running) = changes
            .iter()
            .find(|other| other.specification.environment() == environment && !other.status.is_finished())
        {
            return Err(Refusal::InUse(format!(
                "{environment} is still being changed by {}; changes run one at a time",
                running.metadata.name
            )));
        }
        let reference = ObjectReference::of::<Environment>(project.cloned(), environment.clone());
        let exists = get::<Environment>(self.store, &reference, Consistency::Linearizable).await?.is_some();
        match (change.creates_environment(), exists) {
            (true, true) => {
                Err(Refusal::InUse(format!("{} {environment} already exists", Environment::KIND)))
            }
            (false, false) => {
                Err(Refusal::Unready(format!("there is no {} {environment}", Environment::KIND)))
            }
            _ => Ok(()),
        }
    }

    async fn admit_kubernetes_change(
        &self,
        project: Option<&ProjectName>,
        change: &KubernetesClusterChangeSpecification,
    ) -> Result<(), Refusal> {
        if !matches!(change.request, KubernetesRequest::Delete) {
            return Ok(());
        }
        let controllers = list::<JujuController>(self.store, project, Consistency::Linearizable).await?;
        let hosted: Vec<String> = controllers
            .iter()
            .filter(|controller| {
                controller.specification.substrate.hosting_cluster() == Some(&change.cluster)
            })
            .map(|controller| controller.metadata.name.to_string())
            .collect();
        if hosted.is_empty() {
            return Ok(());
        }
        Err(Refusal::InUse(format!(
            "cluster {} hosts Juju controller {}; delete it first",
            change.cluster,
            hosted.join(", ")
        )))
    }

    async fn admit_juju_change(
        &self,
        project: Option<&ProjectName>,
        change: &JujuControllerChangeSpecification,
    ) -> Result<(), Refusal> {
        let JujuRequest::Create { controller } = &change.request else {
            return Ok(());
        };
        let Some(cluster) = controller.substrate.hosting_cluster() else {
            return Ok(());
        };
        self.require_ready_cluster(project, cluster).await
    }

    async fn require_ready_cluster(
        &self,
        project: Option<&ProjectName>,
        cluster: &ObjectName,
    ) -> Result<(), Refusal> {
        let reference = ObjectReference::of::<KubernetesCluster>(project.cloned(), cluster.clone());
        let found = get::<KubernetesCluster>(self.store, &reference, Consistency::Linearizable).await?;
        match found {
            Some(cluster) if cluster.status.phase == EnvironmentPhase::Ready => Ok(()),
            Some(cluster) => Err(Refusal::Unready(format!(
                "cluster {} is {:?}, not ready",
                cluster.metadata.name, cluster.status.phase
            ))),
            None => {
                Err(Refusal::Unready(format!("there is no Kubernetes cluster {cluster} in this project")))
            }
        }
    }
}

fn refuse_environment_labels(labels: &BTreeMap<String, String>) -> Result<(), Refusal> {
    match owning_environment(labels) {
        Some((label, owner)) => {
            Err(Refusal::Owned(format!("{owner} owns this object ({label}); change it through {owner}")))
        }
        None => Ok(()),
    }
}

fn decode<T: serde::de::DeserializeOwned>(specification: &Value) -> Result<T, Refusal> {
    serde_json::from_value(specification.clone())
        .map_err(|error| Refusal::Store(ObjectStoreError::Malformed(error.to_string())))
}
