use std::collections::BTreeMap;
use std::path::PathBuf;

use amafu_api::ApiClient;
use amafu_domain::kinds::kubernetes_cluster::{
    ClusterMember, KubernetesCluster, KubernetesClusterChange, KubernetesClusterChangeSpecification,
    KubernetesClusterSpecification, KubernetesRequest, KubernetesVersion, NodePool,
};
use amafu_domain::kinds::machine_shape::MachineShape;
use amafu_domain::{Object, ObjectName};
use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use serde_json::json;

use crate::output::{OutputFormat, emit, emit_line};
use crate::resources::{CommonCommand, Tabulated, kebab_name, or_dash, print_objects, run_common};
use crate::step_plans::{Waiting, plan_columns, submit, unique_name};

const KUBERNETES_API_PORT: u16 = 6443;

#[derive(Args)]
pub struct Placement {
    #[arg(long, value_name = "NETWORK")]
    network: ObjectName,
    #[arg(long = "storage-backend", value_name = "BACKEND")]
    storage_backend: ObjectName,
    #[arg(long = "external-network", value_name = "NETWORK", help = "Where the API address comes from")]
    external_network: ObjectName,
}

#[derive(Subcommand)]
pub enum KubernetesCommand {
    #[command(about = "Create a Canonical Kubernetes cluster on Amafu VMs")]
    Create {
        name: ObjectName,
        #[arg(long, value_parser = version, value_name = "VERSION", help = "For example 1.34")]
        version: KubernetesVersion,
        #[arg(long = "control-plane-nodes", default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=5))]
        control_plane_nodes: u8,
        #[arg(long = "control-plane-shape", value_parser = shape, default_value = "medium")]
        control_plane_shape: MachineShape,
        #[arg(long = "pool", value_parser = pool, value_name = "NAME=NODES:SHAPE")]
        pools: Vec<(ObjectName, NodePool)>,
        #[command(flatten)]
        placement: Placement,
        #[command(flatten)]
        waiting: Waiting,
    },
    #[command(about = "Replace every node, control plane first, with one at a newer version")]
    Upgrade {
        name: String,
        #[arg(long, value_parser = version)]
        version: KubernetesVersion,
        #[command(flatten)]
        waiting: Waiting,
    },
    #[command(about = "Grow or shrink a node pool")]
    Scale {
        name: String,
        #[arg(long, value_name = "POOL")]
        pool: ObjectName,
        #[arg(long)]
        nodes: u16,
        #[command(flatten)]
        waiting: Waiting,
    },
    #[command(name = "add-pool", about = "Add a node pool")]
    AddPool {
        name: String,
        #[arg(long, value_name = "POOL")]
        pool: ObjectName,
        #[arg(long)]
        nodes: u16,
        #[arg(long, value_parser = shape)]
        shape: MachineShape,
        #[command(flatten)]
        waiting: Waiting,
    },
    #[command(name = "remove-pool", about = "Drain and remove every node of a pool")]
    RemovePool {
        name: String,
        #[arg(long, value_name = "POOL")]
        pool: ObjectName,
        #[command(flatten)]
        waiting: Waiting,
    },
    #[command(name = "drain-node", about = "Cordon a node and evict its pods")]
    DrainNode {
        name: String,
        node: String,
        #[command(flatten)]
        waiting: Waiting,
    },
    #[command(name = "restart-node", about = "Drain, stop and start a node's VM")]
    RestartNode {
        name: String,
        node: String,
        #[command(flatten)]
        waiting: Waiting,
    },
    #[command(name = "delete", about = "Delete a cluster and every VM, volume and address it owns")]
    Delete {
        name: String,
        #[command(flatten)]
        waiting: Waiting,
    },
    #[command(about = "List clusters")]
    List,
    #[command(about = "Show a cluster")]
    Show { name: String },
    #[command(about = "List the changes made to clusters, or to one cluster")]
    Changes { name: Option<String> },
    #[command(about = "Write a kubeconfig that signs in with your Amafu token; it holds no secret")]
    Kubeconfig {
        name: String,
        #[arg(long, value_name = "PATH", help = "Where to write it; - writes to standard output")]
        file: Option<PathBuf>,
    },
    #[command(about = "Print a short-lived ExecCredential for kubectl")]
    Token { name: String },
}

fn version(text: &str) -> Result<KubernetesVersion, String> {
    KubernetesVersion::try_from(text.to_owned())
}

pub fn shape(text: &str) -> Result<MachineShape, String> {
    serde_json::from_value(json!(text)).map_err(|_| format!("{text:?} is not small, medium, large or xlarge"))
}

fn pool(text: &str) -> Result<(ObjectName, NodePool), String> {
    let invalid = || format!("{text:?} is not NAME=NODES:SHAPE such as general=3:large");
    let (name, size) = text.split_once('=').ok_or_else(invalid)?;
    let (nodes, node_shape) = size.split_once(':').ok_or_else(invalid)?;
    Ok((
        ObjectName::new(name).map_err(|error| error.to_string())?,
        NodePool { nodes: nodes.parse().map_err(|_| invalid())?, shape: shape(node_shape)? },
    ))
}

impl Tabulated for KubernetesCluster {
    const HEADINGS: &'static [&'static str] = &["NAME", "VERSION", "PHASE", "NODES", "ENDPOINT"];

    fn row(cluster: &Object<Self>) -> Vec<String> {
        let ready = cluster.status.nodes.iter().filter(|node| node.is_ready).count();
        vec![
            cluster.metadata.name.to_string(),
            cluster.specification.version.to_string(),
            kebab_name(cluster.status.phase),
            format!("{ready}/{}", cluster.status.nodes.len()),
            or_dash(cluster.status.endpoint.map(|address| address.to_string())),
        ]
    }
}

impl Tabulated for KubernetesClusterChange {
    const HEADINGS: &'static [&'static str] = &["NAME", "CLUSTER", "REQUEST", "PROGRESS", "STEP", "STATE"];

    fn row(change: &Object<Self>) -> Vec<String> {
        let [progress, step, state] = plan_columns(change);
        vec![
            change.metadata.name.to_string(),
            change.specification.cluster.to_string(),
            change.specification.request.name().to_owned(),
            progress,
            step,
            state,
        ]
    }
}

fn members(cluster: &Object<KubernetesCluster>, pool: Option<&ObjectName>) -> Vec<ClusterMember> {
    cluster
        .status
        .nodes
        .iter()
        .filter(|node| pool.is_none() || node.member.pool.as_ref() == pool)
        .map(|node| node.member.clone())
        .collect()
}

fn member_named(cluster: &Object<KubernetesCluster>, node: &str) -> anyhow::Result<ClusterMember> {
    cluster
        .status
        .nodes
        .iter()
        .find(|known| known.vm.as_str() == node)
        .map(|known| known.member.clone())
        .with_context(|| format!("cluster {} has no node {node}", cluster.metadata.name))
}

async fn request(
    client: &ApiClient,
    cluster: &str,
    request: KubernetesRequest,
    waiting: Waiting,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let name = unique_name(request.name(), cluster)?;
    let specification = KubernetesClusterChangeSpecification::new(ObjectName::new(cluster)?, request);
    submit::<KubernetesClusterChange>(client, &name, &specification, waiting.wait, format).await
}

fn kubeconfig(client: &ApiClient, cluster: &Object<KubernetesCluster>) -> anyhow::Result<String> {
    use base64::Engine;
    let address = cluster.status.endpoint.context("the cluster has no API address yet")?;
    let authority = cluster
        .status
        .certificate_authority
        .as_deref()
        .context("the cluster has no certificate authority yet")?;
    let name = cluster.metadata.name.to_string();
    let mut environment = Vec::new();
    for variable in ["AMAFU_ENDPOINT", "AMAFU_CERTIFICATE_AUTHORITY"] {
        if let Ok(value) = std::env::var(variable) {
            environment.push(json!({"name": variable, "value": value}));
        }
    }
    if let Some(project) = client.project() {
        environment.push(json!({"name": "AMAFU_PROJECT", "value": project.to_string()}));
    }
    let document = json!({
        "apiVersion": "v1",
        "kind": "Config",
        "clusters": [{"name": name, "cluster": {
            "server": format!("https://{address}:{KUBERNETES_API_PORT}"),
            "certificate-authority-data": base64::engine::general_purpose::STANDARD.encode(authority),
        }}],
        "users": [{"name": format!("amafu-{name}"), "user": {"exec": {
            "apiVersion": "client.authentication.k8s.io/v1",
            "command": "/snap/bin/amafu",
            "args": ["kubernetes", "token", name],
            "env": environment,
            "interactiveMode": "Never",
            "provideClusterInfo": false,
        }}}],
        "contexts": [{"name": name, "context": {"cluster": name, "user": format!("amafu-{name}")}}],
        "current-context": name,
    });
    Ok(serde_yaml_ng::to_string(&document)?)
}

fn default_kubeconfig_path(client: &ApiClient, name: &str) -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let project = client.project().map_or_else(|| String::from("default"), ToString::to_string);
    Ok(PathBuf::from(home).join("amafu").join(format!("{project}-{name}.kubeconfig")))
}

async fn write_kubeconfig(client: &ApiClient, name: &str, output: Option<PathBuf>) -> anyhow::Result<()> {
    let cluster = client.get::<KubernetesCluster>(name).await?;
    let document = kubeconfig(client, &cluster)?;
    let path = match output {
        Some(path) if path.as_os_str() == "-" => return Ok(emit(&document)?),
        Some(path) => path,
        None => default_kubeconfig_path(client, name)?,
    };
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory).with_context(|| format!("creating {}", directory.display()))?;
    }
    std::fs::write(&path, document).with_context(|| format!("writing {}", path.display()))?;
    emit_line(format!("export KUBECONFIG={}", path.display()))?;
    Ok(())
}

async fn exec_credential(client: &ApiClient, name: &str) -> anyhow::Result<()> {
    let issued = client.platform_token::<KubernetesCluster>(name).await?;
    let expiry = time::OffsetDateTime::from_unix_timestamp(issued.expires_at)?
        .format(&time::format_description::well_known::Rfc3339)?;
    let credential = json!({
        "apiVersion": "client.authentication.k8s.io/v1",
        "kind": "ExecCredential",
        "status": {"token": issued.token, "expirationTimestamp": expiry},
    });
    emit_line(credential.to_string())?;
    Ok(())
}

pub async fn run(client: &ApiClient, command: KubernetesCommand, format: OutputFormat) -> anyhow::Result<()> {
    match command {
        KubernetesCommand::Create {
            name,
            version,
            control_plane_nodes,
            control_plane_shape,
            pools,
            placement,
            waiting,
        } => {
            let node_pools: BTreeMap<ObjectName, NodePool> = pools.into_iter().collect();
            let cluster = KubernetesClusterSpecification {
                version,
                control_plane_nodes,
                control_plane_shape,
                node_pools,
                network: placement.network,
                storage_backend: placement.storage_backend,
                external_network: placement.external_network,
                organization_sign_in: false,
            };
            request(client, name.as_str(), KubernetesRequest::Create { cluster }, waiting, format).await
        }
        KubernetesCommand::Upgrade { name, version, waiting } => {
            let cluster = client.get::<KubernetesCluster>(&name).await?;
            let nodes = members(&cluster, None);
            request(client, &name, KubernetesRequest::Upgrade { version, nodes }, waiting, format).await
        }
        KubernetesCommand::Scale { name, pool, nodes: to, waiting } => {
            let cluster = client.get::<KubernetesCluster>(&name).await?;
            if !cluster.specification.node_pools.contains_key(&pool) {
                bail!("cluster {name} has no pool {pool}");
            }
            let nodes = members(&cluster, Some(&pool));
            request(client, &name, KubernetesRequest::ScalePool { pool, nodes, to }, waiting, format).await
        }
        KubernetesCommand::AddPool { name, pool, nodes, shape, waiting } => {
            let node_pool = NodePool { nodes, shape };
            request(client, &name, KubernetesRequest::AddPool { pool, node_pool }, waiting, format).await
        }
        KubernetesCommand::RemovePool { name, pool, waiting } => {
            let cluster = client.get::<KubernetesCluster>(&name).await?;
            let nodes = members(&cluster, Some(&pool));
            request(client, &name, KubernetesRequest::RemovePool { pool, nodes }, waiting, format).await
        }
        KubernetesCommand::DrainNode { name, node, waiting } => {
            let member = member_named(&client.get::<KubernetesCluster>(&name).await?, &node)?;
            request(client, &name, KubernetesRequest::DrainNode { member }, waiting, format).await
        }
        KubernetesCommand::RestartNode { name, node, waiting } => {
            let member = member_named(&client.get::<KubernetesCluster>(&name).await?, &node)?;
            request(client, &name, KubernetesRequest::RestartNode { member }, waiting, format).await
        }
        KubernetesCommand::Delete { name, waiting } => {
            request(client, &name, KubernetesRequest::Delete, waiting, format).await
        }
        KubernetesCommand::List => run_common::<KubernetesCluster>(client, CommonCommand::List, format).await,
        KubernetesCommand::Show { name } => {
            run_common::<KubernetesCluster>(client, CommonCommand::Show { name }, format).await
        }
        KubernetesCommand::Changes { name } => {
            let changes: Vec<_> = client
                .list::<KubernetesClusterChange>()
                .await?
                .into_iter()
                .filter(|change| {
                    name.as_deref().is_none_or(|name| change.specification.cluster.as_str() == name)
                })
                .collect();
            print_objects(&changes, format)
        }
        KubernetesCommand::Kubeconfig { name, file } => write_kubeconfig(client, &name, file).await,
        KubernetesCommand::Token { name } => exec_credential(client, &name).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pools_are_written_as_name_nodes_and_shape() {
        let (name, node_pool) = pool("general=3:large").unwrap();
        assert_eq!(name.as_str(), "general");
        assert_eq!(node_pool, NodePool { nodes: 3, shape: MachineShape::Large });
        assert!(pool("general=3").is_err());
        assert!(pool("general=x:large").is_err());
        assert!(shape("huge").is_err());
    }
}
