use std::time::Duration;

use amafu_api::ApiClient;
use amafu_domain::kinds::image::{Image, ImageSource, ImageSpecification};
use amafu_domain::kinds::storage_backend::{
    StorageBackend, StorageBackendSpecification, StorageProviderSpecification,
};
use amafu_domain::kinds::vm::{Vm, VmPhase};
use amafu_domain::{Object, ObjectId, ObjectName, OciReference, Resource, Sha256Digest};
use amafu_scenario::{Cluster, Level, ObservedInstance, ScenarioError, ScenarioResult, ensure, eventually};

pub const BUSYBOX: &str =
    "docker.io/library/busybox@sha256:a7dbc319b1438115a207e11930dc4bb3e643dcb3ec0bdd694bb36cd8fad289ec";
pub const DISK_IMAGE_URL: &str =
    "https://cloud-images.ubuntu.com/minimal/releases/noble/release-20261027/ubuntu-24.04-minimal-cloudimg-amd64.img";
pub const DISK_IMAGE_SHA256: &str = "3839f148c5a3b4a736e7dbb0af75594743fa5ebee2a202424ba1a32e5d3b5d2c";
pub const VM_START_BUDGET: Duration = Duration::from_secs(5);
pub const CONTAINER_START_BUDGET: Duration = Duration::from_secs(3);
pub const START_TIMEOUT: Duration = Duration::from_secs(90);
pub const IMAGE_TIMEOUT: Duration = Duration::from_secs(600);
pub const REMOVAL_TIMEOUT: Duration = Duration::from_secs(90);
pub const GUEST_TIMEOUT: Duration = Duration::from_secs(120);
const CONFLICT: u16 = 409;

pub fn name(text: &str) -> Result<ObjectName, ScenarioError> {
    ObjectName::new(text).map_err(|error| ScenarioError::Expectation(error.to_string()))
}

fn parsed<T: std::str::FromStr>(text: &str) -> Result<T, ScenarioError>
where
    T::Err: std::fmt::Display,
{
    text.parse().map_err(|error: T::Err| ScenarioError::Expectation(error.to_string()))
}

pub async fn ready_image(
    client: &ApiClient,
    cluster: &dyn Cluster,
    image_name: &str,
    source: ImageSource,
) -> ScenarioResult {
    client.create::<Image>(&name(image_name)?, &ImageSpecification { source }).await?;
    eventually(cluster, IMAGE_TIMEOUT, "the image is cached and ready", async || {
        Ok(client.get::<Image>(image_name).await?.status.is_ready())
    })
    .await
}

pub fn busybox() -> Result<ImageSource, ScenarioError> {
    Ok(ImageSource::Container { reference: parsed::<OciReference>(BUSYBOX)? })
}

pub fn disk_image() -> Result<ImageSource, ScenarioError> {
    Ok(ImageSource::Disk {
        url: DISK_IMAGE_URL.to_owned(),
        sha256: parsed::<Sha256Digest>(DISK_IMAGE_SHA256)?,
    })
}

pub async fn shared_backend(
    operator: &ApiClient,
    cluster: &dyn Cluster,
    backend_name: &str,
    provider: StorageProviderSpecification,
) -> ScenarioResult {
    match operator
        .create::<StorageBackend>(&name(backend_name)?, &StorageBackendSpecification { provider })
        .await
    {
        Err(error) if error.status() == Some(CONFLICT) => {}
        outcome => {
            outcome?;
        }
    }
    eventually(cluster, START_TIMEOUT, "the storage backend is ready", async || {
        let backend = operator.get::<StorageBackend>(backend_name).await?;
        Ok(backend.status.phase == amafu_domain::kinds::storage_backend::StorageBackendPhase::Ready)
    })
    .await
}

pub async fn every_node_ready(cluster: &dyn Cluster) -> ScenarioResult {
    let operator = amafu_scenario::cluster::client(cluster, amafu_domain::access::Role::Operator).await;
    let expected = cluster.nodes().await.len();
    eventually(cluster, START_TIMEOUT, "every node has registered and is ready", async || {
        let nodes = operator.list::<amafu_domain::kinds::node::Node>().await?;
        Ok(nodes.len() == expected && nodes.iter().all(|node| node.status.is_ready()))
    })
    .await
}

pub async fn vm_in_phase(
    client: &ApiClient,
    cluster: &dyn Cluster,
    vm_name: &str,
    phase: VmPhase,
    timeout: Duration,
) -> Result<Object<Vm>, ScenarioError> {
    eventually(cluster, timeout, "the API reports the VM phase", async || {
        let vm = client.get::<Vm>(vm_name).await?;
        Ok(vm.status.phase == phase && vm.is_status_current())
    })
    .await?;
    Ok(client.get::<Vm>(vm_name).await?)
}

pub async fn started_within_budget(
    client: &ApiClient,
    cluster: &dyn Cluster,
    vm_name: &str,
    budget: Duration,
) -> Result<Object<Vm>, ScenarioError> {
    let clock = cluster.clock();
    let started = clock.now();
    let running = vm_in_phase(client, cluster, vm_name, VmPhase::Running, START_TIMEOUT).await?;
    let elapsed = clock.now() - started;
    if cluster.level() != Level::Simulated {
        ensure(elapsed <= budget, || {
            format!(
                "{vm_name} took {:.1} s to run, over its budget of {} s",
                elapsed.as_seconds_f64(),
                budget.as_secs()
            )
        })?;
    }
    Ok(running)
}

pub async fn instance(cluster: &dyn Cluster, vm_id: ObjectId) -> Result<ObservedInstance, ScenarioError> {
    cluster
        .observe()
        .await?
        .instance(vm_id)
        .cloned()
        .ok_or_else(|| ScenarioError::Expectation(format!("VM {vm_id} has no instance")))
}

pub async fn console_shows(cluster: &dyn Cluster, vm_id: ObjectId, marker: &str) -> ScenarioResult {
    if cluster.level() == Level::Simulated {
        return Ok(());
    }
    eventually(cluster, GUEST_TIMEOUT, &format!("the console shows {marker:?}"), async || {
        Ok(cluster.observe().await?.instance(vm_id).is_some_and(|instance| instance.console.contains(marker)))
    })
    .await
}

pub async fn delete_and_wait<R: Resource>(
    client: &ApiClient,
    cluster: &dyn Cluster,
    object_name: &str,
) -> ScenarioResult
where
    Object<R>: serde::de::DeserializeOwned,
{
    client.delete::<R>(object_name).await?;
    eventually(cluster, REMOVAL_TIMEOUT, &format!("{object_name} is removed"), async || {
        match client.get::<R>(object_name).await {
            Err(error) if error.is_not_found() => Ok(true),
            Err(error) => Err(error.into()),
            Ok(_) => Ok(false),
        }
    })
    .await
}
