use amafu_api::NodeClient;
use amafu_api::node_identity::fingerprint;
use amafu_domain::access::Role;
use amafu_domain::kinds::certificate_revocation::{
    CertificateRevocation, CertificateRevocationSpecification,
};
use amafu_domain::kinds::node::NodeRole;
use amafu_domain::{NodeName, ObjectName, ObjectReference};
use amafu_interfaces::{CoreConnection, SliceChange, StoreError};
use amafu_scenario::cluster::client;
use amafu_scenario::{
    CertificateAuthority, Cluster, Level, ScenarioDefinition, ScenarioError, ScenarioResult, ensure,
};
use futures::{FutureExt, StreamExt};
use serde_json::json;

use crate::support::{delete_and_wait, every_node_ready, name};

pub const ROGUE_NODES_ARE_REFUSED: ScenarioDefinition = ScenarioDefinition {
    name: "nodes without a valid certificate cannot join, subscribe or write, and are audited",
    levels: &[Level::Simulated, Level::VirtualLab],
    node_count: 4,
    run: |cluster| rogue_nodes_are_refused(cluster).boxed_local(),
};

pub const NODES_WRITE_ONLY_THEIR_SHARE: ScenarioDefinition = ScenarioDefinition {
    name: "a node may read and write only its own share of the state",
    levels: &[Level::Simulated, Level::VirtualLab],
    node_count: 4,
    run: |cluster| a_node_writes_only_its_share(cluster).boxed_local(),
};

fn node_name(text: &str) -> Result<NodeName, ScenarioError> {
    NodeName::new(text).map_err(|error| ScenarioError::Expectation(error.to_string()))
}

fn node_reference(text: &str) -> Result<ObjectReference, ScenarioError> {
    Ok(ObjectReference { kind: String::from("Node"), project: None, name: name(text)? })
}

fn lab_failure(error: impl std::fmt::Display) -> ScenarioError {
    ScenarioError::Lab(error.to_string())
}

fn registration(node: &str) -> serde_json::Value {
    json!({
        "metadata": {"name": node, "id": "00000000-0000-4000-8000-000000000001", "version": 1, "generation": 1, "observed_generation": 0},
        "specification": {"roles": ["compute"]},
        "status": {},
    })
}

async fn ensure_refused_and_audited<T: std::fmt::Debug>(
    cluster: &dyn Cluster,
    connection: &NodeClient,
    outcome: Result<T, StoreError>,
    context: &str,
) -> ScenarioResult {
    ensure(matches!(outcome, Err(StoreError::Refused(_))), || {
        format!("{context}: expected a refusal, got {outcome:?}")
    })?;
    let request_id = connection
        .last_request_id()
        .ok_or_else(|| ScenarioError::Expectation(format!("{context}: the refusal carries no request ID")))?;
    ensure(cluster.audit_contains(request_id).await?, || format!("{context}: the refusal is not audited"))
}

async fn ensure_shut_out(
    cluster: &dyn Cluster,
    connection: &NodeClient,
    description: &str,
) -> ScenarioResult {
    let subscription = connection.subscribe().await.map(|_| ());
    ensure_refused_and_audited(cluster, connection, subscription, &format!("{description} subscribing"))
        .await?;
    let joined =
        connection.compare_and_swap(&node_reference("node-3")?, None, Some(registration("node-3"))).await;
    ensure_refused_and_audited(cluster, connection, joined, &format!("{description} registering a node"))
        .await?;
    let listed = connection.list("Vm", None).await;
    ensure_refused_and_audited(cluster, connection, listed, &format!("{description} listing VMs")).await
}

async fn rogue_nodes_are_refused(cluster: &dyn Cluster) -> ScenarioResult {
    let anonymous = cluster.node_connection(None).await?;
    ensure_shut_out(cluster, &anonymous, "a node without a certificate").await?;

    let foreign_authority = CertificateAuthority::generate("another cluster")?;
    let foreign = foreign_authority.issue_node(&node_name("node-3")?, &[NodeRole::Compute])?;
    let impostor = cluster.node_connection(Some(&foreign)).await?;
    ensure_shut_out(cluster, &impostor, "a node with another cluster's certificate").await?;

    let issued = cluster.certificate_authority().issue_node(&node_name("node-9")?, &[NodeRole::Compute])?;
    let former = cluster.node_connection(Some(&issued)).await?;
    let mut changes = former.subscribe().await.map_err(lab_failure)?;
    let first = changes.next().await;
    ensure(matches!(first, Some(Ok(SliceChange::Snapshot { .. }))), || {
        format!("a valid node first receives a snapshot, got {first:?}")
    })?;
    drop(changes);
    let admin = client(cluster, Role::Admin).await;
    let revocation = CertificateRevocationSpecification {
        fingerprint: fingerprint(&issued.certificate_der),
        reason: String::from("decommissioned"),
    };
    admin
        .create::<CertificateRevocation>(&ObjectName::new("node-9").map_err(lab_failure)?, &revocation)
        .await?;
    ensure_shut_out(cluster, &former, "a node with a revoked certificate").await?;
    delete_and_wait::<CertificateRevocation>(&admin, cluster, "node-9").await
}

async fn a_node_writes_only_its_share(cluster: &dyn Cluster) -> ScenarioResult {
    every_node_ready(cluster).await?;
    let identity = cluster.certificate_authority().issue_node(&node_name("node-8")?, &[NodeRole::Compute])?;
    let connection = cluster.node_connection(Some(&identity)).await?;
    let others = connection.list("Node", None).await.map_err(lab_failure)?;
    ensure(others.is_empty(), || format!("an unregistered node sees other nodes: {others:?}"))?;
    let other_node = connection.read(&node_reference("node-0")?).await;
    ensure_refused_and_audited(cluster, &connection, other_node, "reading another node").await?;
    let hijack =
        connection.compare_and_swap(&node_reference("node-0")?, None, Some(registration("node-0"))).await;
    ensure_refused_and_audited(cluster, &connection, hijack, "registering as another node").await?;
    let elevated = json!({
        "metadata": {"name": "node-8", "id": "00000000-0000-4000-8000-000000000008", "version": 1, "generation": 1, "observed_generation": 0},
        "specification": {"roles": ["core", "compute"]},
        "status": {},
    });
    let promoted = connection.compare_and_swap(&node_reference("node-8")?, None, Some(elevated)).await;
    ensure_refused_and_audited(cluster, &connection, promoted, "claiming a role the certificate lacks")
        .await?;
    let vm = ObjectReference {
        kind: String::from("Vm"),
        project: Some(amafu_domain::ProjectName::new("project-a").map_err(lab_failure)?),
        name: name("smuggled")?,
    };
    let smuggled = connection
        .compare_and_swap(
            &vm,
            None,
            Some(json!({"metadata": {"name": "smuggled", "project": "project-a"}, "specification": {}})),
        )
        .await;
    ensure_refused_and_audited(cluster, &connection, smuggled, "creating a VM").await
}
