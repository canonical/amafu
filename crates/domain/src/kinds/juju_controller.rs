use std::fmt;

use serde::{Deserialize, Serialize};

use crate::access::ProjectPolicy;
use crate::kinds::environment::{EnvironmentChange, EnvironmentPhase, EnvironmentStatus};
use crate::kinds::machine_shape::MachineShape;
use crate::kinds::operation::OperationStatus;
use crate::name::ObjectName;
use crate::resource::{Resource, Scope};
use crate::step_plan::{PlannedStep, StepPlan};
use crate::validation::{Validate, ValidationError};

pub const CONTROLLER_COUNTS: [u8; 2] = [1, 3];
pub const HIGHLY_AVAILABLE_CONTROLLERS: u8 = 3;
pub const MAXIMUM_PODS: u8 = 16;
pub const MAXIMUM_UNITS: u16 = 100;

#[must_use]
pub fn is_valid_pod_count(pods: u8) -> bool {
    pods == 1 || (3..=MAXIMUM_PODS).contains(&pods)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JujuController;

impl Resource for JujuController {
    const KIND: &'static str = "JujuController";
    const PLURAL_NAME: &'static str = "juju-controllers";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Project;
    const PROJECT_POLICY: ProjectPolicy = ProjectPolicy::ManagedByNodes;
    type Specification = JujuControllerSpecification;
    type Status = JujuControllerStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct JujuVersion {
    major: u16,
    minor: u16,
    patch: u16,
}

impl JujuVersion {
    #[must_use]
    pub fn new(major: u16, minor: u16, patch: u16) -> Self {
        Self { major, minor, patch }
    }

    #[must_use]
    pub fn snap_channel(&self) -> String {
        format!("{}.{}/stable", self.major, self.minor)
    }
}

impl fmt::Display for JujuVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl TryFrom<String> for JujuVersion {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let invalid = || format!("{text:?} is not a Juju version such as 4.2.1");
        let parts: Vec<u16> =
            text.split('.').map(str::parse).collect::<Result<_, _>>().map_err(|_| invalid())?;
        match parts.as_slice() {
            [major, minor, patch] => Ok(Self::new(*major, *minor, *patch)),
            _ => Err(invalid()),
        }
    }
}

impl From<JujuVersion> for String {
    fn from(version: JujuVersion) -> Self {
        version.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "on", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Substrate {
    Pods {
        pods: u8,
        controllers: u8,
        shape: MachineShape,
        network: ObjectName,
        storage_backend: ObjectName,
        external_network: ObjectName,
    },
    Kubernetes {
        cluster: ObjectName,
    },
}

impl Substrate {
    #[must_use]
    pub fn hosting_cluster(&self) -> Option<&ObjectName> {
        match self {
            Self::Kubernetes { cluster } => Some(cluster),
            Self::Pods { .. } => None,
        }
    }

    #[must_use]
    pub fn controller_count(&self) -> u8 {
        match self {
            Self::Pods { controllers, .. } => *controllers,
            Self::Kubernetes { .. } => 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SignInMode {
    #[default]
    Amafu,
    Jaas,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JujuControllerSpecification {
    pub version: JujuVersion,
    pub substrate: Substrate,
    pub first_model: ObjectName,
    #[serde(default)]
    pub sign_in: SignInMode,
}

impl JujuControllerSpecification {
    fn check(&self, field: &str, error: &mut ValidationError) {
        if let Substrate::Pods { pods, controllers, .. } = &self.substrate {
            error.check(is_valid_pod_count(*pods), &format!("{field}.substrate.pods"), POD_COUNT_RULE);
            error.check(
                CONTROLLER_COUNTS.contains(controllers),
                &format!("{field}.substrate.controllers"),
                "must be 1 or 3",
            );
            error.check(
                controllers <= pods,
                &format!("{field}.substrate.controllers"),
                "cannot outnumber the pods",
            );
        }
    }
}

impl Validate for JujuControllerSpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        self.check("specification", &mut error);
        error.into_result()
    }
}

const POD_COUNT_RULE: &str = "must be 1, or between 3 and 16 so that the LXD cluster keeps a quorum";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JujuModelStatus {
    pub name: ObjectName,
    pub uuid: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct JujuControllerStatus {
    #[serde(default)]
    pub phase: EnvironmentPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate_authority: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<JujuModelStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<JujuVersion>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "time::serde::rfc3339::option")]
    pub credential_rotated_at: Option<time::OffsetDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl EnvironmentStatus for JujuControllerStatus {
    fn phase(&self) -> EnvironmentPhase {
        self.phase
    }

    fn failed(&self, reason: &str) -> Self {
        Self { phase: EnvironmentPhase::Failed, message: Some(reason.to_owned()), ..self.clone() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JujuControllerChange;

impl Resource for JujuControllerChange {
    const KIND: &'static str = "JujuControllerChange";
    const PLURAL_NAME: &'static str = "juju-controller-changes";
    const VERSION: u32 = 1;
    const SCOPE: Scope = Scope::Project;
    type Specification = JujuControllerChangeSpecification;
    type Status = OperationStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Deployment {
    pub model: ObjectName,
    pub application: ObjectName,
    pub charm: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    pub units: u16,
    #[serde(default)]
    pub expose: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum JujuRequest {
    Create { controller: JujuControllerSpecification },
    Delete,
    Upgrade { version: JujuVersion },
    AddModel { model: ObjectName },
    DestroyModel { model: ObjectName },
    EnableHa,
    ScalePods { pods: u8 },
    Deploy { deployment: Deployment },
    ScaleApplication { model: ObjectName, application: ObjectName, units: u16 },
    RemoveUnits { model: ObjectName, application: ObjectName, units: Vec<String> },
    RemoveApplication { model: ObjectName, application: ObjectName },
    RotateCredential,
}

impl JujuRequest {
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Create { .. } => "create",
            Self::Delete => "delete",
            Self::Upgrade { .. } => "upgrade",
            Self::AddModel { .. } => "add-model",
            Self::DestroyModel { .. } => "destroy-model",
            Self::EnableHa => "enable-ha",
            Self::ScalePods { .. } => "scale-pods",
            Self::Deploy { .. } => "deploy",
            Self::ScaleApplication { .. } => "scale-application",
            Self::RemoveUnits { .. } => "remove-units",
            Self::RemoveApplication { .. } => "remove-application",
            Self::RotateCredential => "rotate-credential",
        }
    }

    #[must_use]
    pub fn needs_pods(&self) -> bool {
        matches!(self, Self::EnableHa | Self::ScalePods { .. })
    }

    #[must_use]
    pub fn steps(&self) -> Vec<JujuStep> {
        match self {
            Self::Create { controller } => creation_steps(controller),
            Self::Delete => vec![JujuStep::DestroyModels, JujuStep::DeleteResources],
            Self::Upgrade { version } => vec![
                JujuStep::UpgradeController { version: version.clone() },
                JujuStep::UpgradeModels { version: version.clone() },
            ],
            Self::AddModel { model } => vec![JujuStep::AddModel { model: model.clone() }],
            Self::DestroyModel { model } => vec![JujuStep::DestroyModel { model: model.clone() }],
            Self::EnableHa => vec![JujuStep::EnableHa],
            Self::ScalePods { pods } => {
                vec![JujuStep::RecordPodCount { pods: *pods }, JujuStep::FormCluster, JujuStep::RetirePods]
            }
            Self::Deploy { deployment } => {
                let deploy = JujuStep::Deploy { deployment: deployment.clone() };
                let expose = deployment.expose.then(|| JujuStep::Expose {
                    model: deployment.model.clone(),
                    application: deployment.application.clone(),
                });
                std::iter::once(deploy).chain(expose).collect()
            }
            Self::ScaleApplication { model, application, units } => vec![JujuStep::ScaleApplication {
                model: model.clone(),
                application: application.clone(),
                units: *units,
            }],
            Self::RemoveUnits { model, application, units } => vec![JujuStep::RemoveUnits {
                model: model.clone(),
                application: application.clone(),
                units: units.clone(),
            }],
            Self::RemoveApplication { model, application } => {
                vec![JujuStep::RemoveApplication { model: model.clone(), application: application.clone() }]
            }
            Self::RotateCredential => vec![JujuStep::RotateCredential],
        }
    }
}

fn creation_steps(controller: &JujuControllerSpecification) -> Vec<JujuStep> {
    let substrate_steps = match &controller.substrate {
        Substrate::Pods { .. } => vec![
            JujuStep::PrepareNetworkAccess,
            JujuStep::ImportImage,
            JujuStep::CreateFounderPod,
            JujuStep::FormCluster,
            JujuStep::Bootstrap,
            JujuStep::ExposeController,
            JujuStep::RecordController,
        ],
        Substrate::Kubernetes { .. } => vec![
            JujuStep::CheckHostCluster,
            JujuStep::ImportImage,
            JujuStep::CreateClusterServiceAccount,
            JujuStep::Bootstrap,
            JujuStep::AwaitControllerService,
            JujuStep::RecordController,
            JujuStep::CleanUpBootstrap,
        ],
    };
    let high_availability = (controller.substrate.controller_count() > 1).then_some(JujuStep::EnableHa);
    std::iter::once(JujuStep::RecordSpecification)
        .chain(substrate_steps)
        .chain(high_availability)
        .chain([JujuStep::AddModel { model: controller.first_model.clone() }, JujuStep::MarkReady])
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "kebab-case", deny_unknown_fields)]
pub enum JujuStep {
    RecordSpecification,
    PrepareNetworkAccess,
    ImportImage,
    CreateFounderPod,
    FormCluster,
    CheckHostCluster,
    CreateClusterServiceAccount,
    Bootstrap,
    AwaitControllerService,
    ExposeController,
    RecordController,
    EnableHa,
    CleanUpBootstrap,
    AddModel { model: ObjectName },
    MarkReady,
    DestroyModels,
    DeleteResources,
    UpgradeController { version: JujuVersion },
    UpgradeModels { version: JujuVersion },
    DestroyModel { model: ObjectName },
    RecordPodCount { pods: u8 },
    RetirePods,
    Deploy { deployment: Deployment },
    Expose { model: ObjectName, application: ObjectName },
    ScaleApplication { model: ObjectName, application: ObjectName, units: u16 },
    RemoveUnits { model: ObjectName, application: ObjectName, units: Vec<String> },
    RemoveApplication { model: ObjectName, application: ObjectName },
    RotateCredential,
}

impl PlannedStep for JujuStep {
    fn name(&self) -> &'static str {
        match self {
            Self::RecordSpecification => "record-specification",
            Self::PrepareNetworkAccess => "prepare-network-access",
            Self::ImportImage => "import-image",
            Self::CreateFounderPod => "create-founder-pod",
            Self::FormCluster => "form-cluster",
            Self::CheckHostCluster => "check-host-cluster",
            Self::CreateClusterServiceAccount => "create-cluster-service-account",
            Self::Bootstrap => "bootstrap",
            Self::AwaitControllerService => "await-controller-service",
            Self::ExposeController => "expose-controller",
            Self::RecordController => "record-controller",
            Self::EnableHa => "enable-ha",
            Self::CleanUpBootstrap => "clean-up-bootstrap",
            Self::AddModel { .. } => "add-model",
            Self::MarkReady => "mark-ready",
            Self::DestroyModels => "destroy-models",
            Self::DeleteResources => "delete-resources",
            Self::UpgradeController { .. } => "upgrade-controller",
            Self::UpgradeModels { .. } => "upgrade-models",
            Self::DestroyModel { .. } => "destroy-model",
            Self::RecordPodCount { .. } => "record-pod-count",
            Self::RetirePods => "retire-pods",
            Self::Deploy { .. } => "deploy",
            Self::Expose { .. } => "expose",
            Self::ScaleApplication { .. } => "scale-application",
            Self::RemoveUnits { .. } => "remove-units",
            Self::RemoveApplication { .. } => "remove-application",
            Self::RotateCredential => "rotate-credential",
        }
    }

    fn can_be_undone(&self) -> bool {
        matches!(self, Self::CheckHostCluster | Self::ImportImage)
    }

    fn can_be_abandoned(&self) -> bool {
        !matches!(
            self,
            Self::DestroyModels | Self::DeleteResources | Self::DestroyModel { .. } | Self::RetirePods
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestedJujuControllerChangeSpecification {
    controller: ObjectName,
    request: JujuRequest,
    #[serde(default)]
    steps: Option<Vec<JujuStep>>,
    #[serde(default)]
    is_paused: bool,
    #[serde(default)]
    is_cancelled: bool,
}

impl From<RequestedJujuControllerChangeSpecification> for JujuControllerChangeSpecification {
    fn from(requested: RequestedJujuControllerChangeSpecification) -> Self {
        let steps = requested.steps.unwrap_or_else(|| requested.request.steps());
        Self {
            controller: requested.controller,
            request: requested.request,
            steps,
            is_paused: requested.is_paused,
            is_cancelled: requested.is_cancelled,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "RequestedJujuControllerChangeSpecification")]
pub struct JujuControllerChangeSpecification {
    pub controller: ObjectName,
    pub request: JujuRequest,
    pub steps: Vec<JujuStep>,
    #[serde(default)]
    pub is_paused: bool,
    #[serde(default)]
    pub is_cancelled: bool,
}

impl JujuControllerChangeSpecification {
    #[must_use]
    pub fn new(controller: ObjectName, request: JujuRequest) -> Self {
        let steps = request.steps();
        Self { controller, request, steps, is_paused: false, is_cancelled: false }
    }
}

impl EnvironmentChange for JujuControllerChangeSpecification {
    fn environment(&self) -> &ObjectName {
        &self.controller
    }

    fn creates_environment(&self) -> bool {
        matches!(self.request, JujuRequest::Create { .. })
    }
}

impl StepPlan for JujuControllerChangeSpecification {
    type Step = JujuStep;

    fn steps(&self) -> &[JujuStep] {
        &self.steps
    }

    fn is_paused(&self) -> bool {
        self.is_paused
    }

    fn is_cancelled(&self) -> bool {
        self.is_cancelled
    }

    fn cancel(&mut self) {
        self.is_cancelled = true;
    }
}

impl Validate for JujuControllerChangeSpecification {
    fn validate(&self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(
            self.steps == self.request.steps(),
            "specification.steps",
            "must be the steps of the request",
        );
        match &self.request {
            JujuRequest::Create { controller } => controller.check("specification.controller", &mut error),
            JujuRequest::Deploy { deployment } => error.check(
                (1..=MAXIMUM_UNITS).contains(&deployment.units),
                "specification.deployment.units",
                "must be between 1 and 100",
            ),
            JujuRequest::ScaleApplication { units, .. } => {
                error.check(
                    (1..=MAXIMUM_UNITS).contains(units),
                    "specification.units",
                    "must be between 1 and 100",
                );
            }
            JujuRequest::ScalePods { pods } => {
                error.check(is_valid_pod_count(*pods), "specification.pods", POD_COUNT_RULE);
            }
            _ => {}
        }
        error.into_result()
    }

    fn validate_update(&self, previous: &Self) -> Result<(), ValidationError> {
        let mut error = ValidationError::new();
        error.check(self.controller == previous.controller, "specification.controller", "cannot change");
        error.check(self.request == previous.request, "specification.request", "cannot change");
        error.check(self.steps == previous.steps, "specification.steps", "cannot change");
        error.check(
            self.is_cancelled || !previous.is_cancelled,
            "specification.is_cancelled",
            "cannot be withdrawn",
        );
        error.into_result()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(value: &str) -> ObjectName {
        ObjectName::new(value).unwrap()
    }

    fn on_pods(pods: u8, controllers: u8) -> JujuControllerSpecification {
        JujuControllerSpecification {
            version: JujuVersion::new(4, 2, 1),
            substrate: Substrate::Pods {
                pods,
                controllers,
                shape: MachineShape::Medium,
                network: name("app"),
                storage_backend: name("shared"),
                external_network: name("public"),
            },
            first_model: name("default"),
            sign_in: SignInMode::Amafu,
        }
    }

    fn names(steps: &[JujuStep]) -> Vec<&'static str> {
        steps.iter().map(PlannedStep::name).collect()
    }

    fn refuses(request: JujuRequest) -> bool {
        JujuControllerChangeSpecification::new(name("ctl"), request).validate().is_err()
    }

    #[test]
    fn a_highly_available_controller_on_pods_forms_the_cluster_then_enables_ha() {
        let steps = JujuRequest::Create { controller: on_pods(3, 3) }.steps();
        assert_eq!(
            names(&steps),
            [
                "record-specification",
                "prepare-network-access",
                "import-image",
                "create-founder-pod",
                "form-cluster",
                "bootstrap",
                "expose-controller",
                "record-controller",
                "enable-ha",
                "add-model",
                "mark-ready"
            ]
        );
    }

    #[test]
    fn a_controller_on_kubernetes_is_bootstrapped_through_the_cluster_without_pods() {
        let mut controller = on_pods(1, 1);
        controller.substrate = Substrate::Kubernetes { cluster: name("platform") };
        let steps = JujuRequest::Create { controller }.steps();
        assert!(names(&steps).contains(&"create-cluster-service-account"));
        assert!(names(&steps).contains(&"clean-up-bootstrap"));
        assert!(!names(&steps).contains(&"create-founder-pod"));
    }

    #[test]
    fn a_deployment_is_exposed_only_when_asked() {
        let mut deployment = Deployment {
            model: name("default"),
            application: name("postgresql"),
            charm: String::from("postgresql"),
            channel: Some(String::from("16/stable")),
            units: 3,
            expose: false,
        };
        assert_eq!(names(&JujuRequest::Deploy { deployment: deployment.clone() }.steps()), ["deploy"]);
        deployment.expose = true;
        assert_eq!(names(&JujuRequest::Deploy { deployment }.steps()), ["deploy", "expose"]);
    }

    #[test]
    fn pod_clusters_keep_a_quorum_and_never_hold_fewer_pods_than_controllers() {
        for pods in [0, 2, 17] {
            assert!(refuses(JujuRequest::Create { controller: on_pods(pods, 1) }));
            assert!(refuses(JujuRequest::ScalePods { pods }));
        }
        assert!(refuses(JujuRequest::Create { controller: on_pods(1, 3) }));
        assert!(refuses(JujuRequest::Create { controller: on_pods(3, 2) }));
        assert!(!refuses(JujuRequest::Create { controller: on_pods(3, 3) }));
        assert!(!refuses(JujuRequest::ScalePods { pods: 4 }));
    }

    #[test]
    fn an_application_scales_to_between_one_and_a_hundred_units() {
        let scale =
            |units| JujuRequest::ScaleApplication { model: name("default"), application: name("web"), units };
        assert!(refuses(scale(0)));
        assert!(refuses(scale(101)));
        assert!(!refuses(scale(5)));
    }

    #[test]
    fn a_change_survives_a_trip_through_json() {
        let change = JujuControllerChangeSpecification::new(
            name("ctl"),
            JujuRequest::Create { controller: on_pods(3, 1) },
        );
        let json = serde_json::to_value(&change).unwrap();
        assert_eq!(json["request"]["controller"]["substrate"]["on"], "pods");
        assert_eq!(serde_json::from_value::<JujuControllerChangeSpecification>(json).unwrap(), change);
    }

    #[test]
    fn versions_read_and_print_as_three_numbers() {
        assert_eq!(JujuVersion::try_from(String::from("4.2.1")).unwrap().snap_channel(), "4.2/stable");
        assert!(JujuVersion::try_from(String::from("4.2")).is_err());
    }
}
