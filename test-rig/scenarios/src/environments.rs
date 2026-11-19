use amafu_api::ApiClient;
use amafu_domain::access::Role;
use amafu_domain::kinds::juju_controller::{
    JujuController, JujuControllerChange, JujuControllerChangeSpecification, JujuControllerSpecification,
    JujuRequest, JujuVersion, SignInMode, Substrate,
};
use amafu_domain::kinds::kubernetes_cluster::{
    KubernetesClusterChange, KubernetesClusterChangeSpecification, KubernetesRequest,
};
use amafu_domain::kinds::security_group::SecurityGroup;
use amafu_domain::{ObjectName, ProjectName};
use amafu_scenario::{
    Cluster, Level, ScenarioDefinition, ScenarioError, ScenarioResult, ensure, ensure_equal,
};
use futures::FutureExt;
use http::Method;

const PROJECT: &str = "project-a";
const CREATED: u16 = 201;
const FORBIDDEN: u16 = 403;
const CONFLICT: u16 = 409;
const UNPROCESSABLE: u16 = 422;

pub const ENVIRONMENTS_ARE_GUARDED: ScenarioDefinition = ScenarioDefinition {
    name: "objects owned by an environment are protected and environment changes are admitted against the store",
    levels: &[Level::Simulated, Level::VirtualLab],
    node_count: 1,
    run: |cluster| environments_are_guarded(cluster).boxed_local(),
};

fn name(value: &str) -> Result<ObjectName, ScenarioError> {
    ObjectName::new(value).map_err(|error| ScenarioError::Expectation(error.to_string()))
}

async fn create_group(member: &ApiClient, group: &str, label: &str) -> Result<u16, ScenarioError> {
    let (key, value) = label.split_once('=').unwrap_or_default();
    let body = serde_json::json!({
        "metadata": {"name": group, "labels": {key: value}},
        "specification": {"rules": []},
    });
    let path = format!("/v1/security-groups?project={PROJECT}");
    Ok(member.raw(Method::POST, &path, Some(body.to_string().into_bytes())).await?.status)
}

fn controller_on(cluster: &str) -> Result<JujuControllerSpecification, ScenarioError> {
    Ok(JujuControllerSpecification {
        version: JujuVersion::new(4, 2, 1),
        substrate: Substrate::Kubernetes { cluster: name(cluster)? },
        first_model: name("default")?,
        sign_in: SignInMode::Amafu,
    })
}

fn refused_with(result: Result<impl Sized, amafu_api::ClientError>) -> Option<u16> {
    result.err().and_then(|error| error.status())
}

async fn environments_are_guarded(cluster: &dyn Cluster) -> ScenarioResult {
    let project = ProjectName::new(PROJECT).map_err(|error| ScenarioError::Expectation(error.to_string()))?;
    let member = cluster.client_in_project(Role::Member, &project).await;
    let admin = cluster.client_in_project(Role::Admin, &project).await;

    let owned = create_group(&member, "owned", "amafu.io/kubernetes-cluster=platform").await?;
    ensure_equal(&owned, &FORBIDDEN, || {
        String::from("a member cannot create an object an environment owns")
    })?;
    let ordinary = create_group(&member, "web", "tier=web").await?;
    ensure_equal(&ordinary, &CREATED, || String::from("an ordinary label is accepted"))?;
    let web = member.list_labelled::<SecurityGroup>("tier=web").await?;
    ensure(web.iter().any(|group| group.metadata.name.as_str() == "web"), || {
        String::from("a label selector finds the labelled object")
    })?;
    ensure(member.list_labelled::<SecurityGroup>("tier=database").await?.is_empty(), || {
        String::from("a label selector leaves out objects without the label")
    })?;

    admin.create::<JujuController>(&name("observability")?, &controller_on("platform")?).await?;
    let delete_host = KubernetesClusterChangeSpecification::new(name("platform")?, KubernetesRequest::Delete);
    let refusal =
        refused_with(member.create::<KubernetesClusterChange>(&name("delete-platform")?, &delete_host).await);
    ensure_equal(&refusal, &Some(CONFLICT), || {
        String::from("a cluster that hosts a Juju controller cannot be deleted")
    })?;

    let on_missing = JujuControllerChangeSpecification::new(
        name("orphan")?,
        JujuRequest::Create { controller: controller_on("missing")? },
    );
    let refusal =
        refused_with(member.create::<JujuControllerChange>(&name("create-orphan")?, &on_missing).await);
    ensure_equal(&refusal, &Some(UNPROCESSABLE), || {
        String::from("a controller on Kubernetes needs a ready cluster")
    })?;

    admin.delete::<JujuController>("observability").await?;
    member.delete::<SecurityGroup>("web").await?;
    Ok(())
}
