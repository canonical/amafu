use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use amafu_api::{ApiClient, HttpTransport, Transport};
use amafu_domain::install::InstallPlan;
use amafu_domain::kinds::operation::{Operation, OperationOutcome, OperationSpecification};
use amafu_domain::{Change, Object, ObjectId, ObjectName, ObjectReference};
use amafu_interfaces::objects::{apply, get};
use amafu_interfaces::{Clock, Consistency, Fence, MachineProvisioner, Store, TrustedImageKeys};
use amafu_reconciler::{OperationContext, Progress, advance};

use crate::configuration::{ClusterCredentials, api_endpoint};
use crate::description::InstallDescription;
use crate::journal::JournalStore;
use crate::steps::InstallSteps;

const JOURNAL_FILE: &str = "install-journal.json";
const CREDENTIALS_FILE: &str = "cluster-credentials.json";
const INSTALL_OPERATION: &str = "install";
const WAITING_POLL: Duration = Duration::from_secs(5);
const RETRY_DELAY: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("the install state cannot be kept: {0}")]
    Journal(String),
    #[error("the cluster credentials are unusable: {0}")]
    Credentials(String),
    #[error("the install failed: {0}")]
    Failed(String),
    #[error("the install was cancelled")]
    Cancelled,
}

pub struct InstallDirectory {
    root: PathBuf,
}

impl InstallDirectory {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[must_use]
    pub fn journal(&self) -> PathBuf {
        self.root.join(JOURNAL_FILE)
    }

    #[must_use]
    pub fn credentials(&self) -> PathBuf {
        self.root.join(CREDENTIALS_FILE)
    }
}

pub fn cluster_client(
    description: &InstallDescription,
    credentials: &ClusterCredentials,
) -> Result<ApiClient, InstallError> {
    let machine = description
        .core_members()
        .next()
        .ok_or_else(|| InstallError::Credentials(String::from("the cluster has no core member")))?;
    let transport =
        HttpTransport::new(&api_endpoint(machine), Some(credentials.authority.certificate_pem.as_bytes()))
            .map_err(|error| InstallError::Credentials(error.to_string()))?;
    Ok(ApiClient::new(Arc::new(transport) as Arc<dyn Transport>, Some(credentials.admin_token.clone())))
}

fn journal_failure(error: impl std::fmt::Display) -> InstallError {
    InstallError::Journal(error.to_string())
}

fn install_reference() -> Result<ObjectReference, InstallError> {
    Ok(ObjectReference::of::<Operation>(None, ObjectName::new(INSTALL_OPERATION).map_err(journal_failure)?))
}

async fn install_operation(store: &dyn Store, plan: InstallPlan) -> Result<Object<Operation>, InstallError> {
    let reference = install_reference()?;
    if let Some(existing) =
        get::<Operation>(store, &reference, Consistency::Linearizable).await.map_err(journal_failure)?
    {
        return Ok(existing);
    }
    let created = Change::Create {
        id: ObjectId::from_uuid(uuid::Uuid::new_v4()),
        name: reference.name.clone(),
        project: None,
        labels: BTreeMap::new(),
        specification: OperationSpecification::install(plan),
    };
    apply::<Operation>(store, &reference, created).await.map_err(journal_failure)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallProgress {
    Advanced,
    Waiting,
    Retrying,
    Succeeded,
}

pub struct InstallSession {
    journal: JournalStore,
    steps: InstallSteps,
    clock: Arc<dyn Clock>,
    credentials: ClusterCredentials,
}

impl InstallSession {
    pub async fn open(
        description: InstallDescription,
        directory: &InstallDirectory,
        provisioner: Arc<dyn MachineProvisioner>,
        cluster: Option<ApiClient>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, InstallError> {
        let credentials =
            ClusterCredentials::load_or_create(&directory.credentials(), &description.cluster.name)
                .map_err(|error| InstallError::Credentials(error.to_string()))?;
        let keys =
            TrustedImageKeys::from_hexadecimal(description.cluster.image_keys.iter().map(String::as_str))
                .map_err(|error| InstallError::Credentials(error.to_string()))?;
        let journal = JournalStore::open(&directory.journal()).map_err(journal_failure)?;
        let plan =
            InstallPlan { image: description.cluster.image.clone(), nodes: description.ordered_nodes() };
        install_operation(&journal, plan).await?;
        let cluster = match cluster {
            Some(cluster) => cluster,
            None => cluster_client(&description, &credentials)?,
        };
        let steps = InstallSteps {
            cluster: Arc::new(cluster),
            description: Arc::new(description),
            credentials: Arc::new(credentials.clone()),
            provisioner,
            image_keys: Arc::new(keys),
        };
        Ok(Self { journal, steps, clock, credentials })
    }

    #[must_use]
    pub fn credentials(&self) -> &ClusterCredentials {
        &self.credentials
    }

    pub async fn operation(&self) -> Result<Object<Operation>, InstallError> {
        get::<Operation>(&self.journal, &install_reference()?, Consistency::Linearizable)
            .await
            .map_err(journal_failure)?
            .ok_or_else(|| InstallError::Journal(String::from("the install operation disappeared")))
    }

    pub async fn advance(&self) -> Result<InstallProgress, InstallError> {
        let operation = self.operation().await?;
        match &operation.status.outcome {
            Some(OperationOutcome::Succeeded) => return Ok(InstallProgress::Succeeded),
            Some(OperationOutcome::Failed { reason }) => return Err(InstallError::Failed(reason.clone())),
            Some(OperationOutcome::Cancelled) => return Err(InstallError::Cancelled),
            None => {}
        }
        let fence = Fence { name: String::from("installer"), token: 1 };
        let context = OperationContext {
            operation: &operation,
            store: &self.journal,
            clock: self.clock.as_ref(),
            fence: &fence,
        };
        match advance(&context, &self.steps).await {
            Ok(Progress::Advanced | Progress::Finished) => {
                let step = operation.specification.steps.get(operation.status.completed_steps);
                tracing::info!(
                    step = step.map(amafu_domain::kinds::operation::StepSpecification::name),
                    "the install advanced"
                );
                Ok(InstallProgress::Advanced)
            }
            Ok(Progress::Waiting | Progress::Idle) => Ok(InstallProgress::Waiting),
            Err(error) => {
                tracing::warn!(%error, "an install step failed; trying again");
                Ok(InstallProgress::Retrying)
            }
        }
    }

    pub async fn run_to_completion(&self) -> Result<(), InstallError> {
        loop {
            match self.advance().await? {
                InstallProgress::Succeeded => return Ok(()),
                InstallProgress::Advanced => {}
                InstallProgress::Waiting => self.clock.sleep(WAITING_POLL).await,
                InstallProgress::Retrying => self.clock.sleep(RETRY_DELAY).await,
            }
        }
    }
}
