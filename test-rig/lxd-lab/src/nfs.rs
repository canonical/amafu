use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use anyhow::bail;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::lxd::LxdClient;
use crate::topology::LabSettings;

pub const NFS_INSTANCE: &str = "nfs-server";
pub const NFS_EXPORT: &str = "/srv/amafu";
const NFS_PORT: u16 = 2049;
const IMAGE_SERVER: &str = "https://images.lxd.canonical.com";
const IMAGE_ALIAS: &str = "ubuntu/24.04/cloud";
const READY_TIMEOUT: Duration = Duration::from_secs(600);
const READY_POLL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabNfsServer {
    pub address: Ipv4Addr,
    pub export: String,
    pub proxy_port: u16,
}

impl LabNfsServer {
    #[must_use]
    pub fn standard(settings: &LabSettings, proxy_port: u16) -> Self {
        let octets = settings.gateway.octets();
        Self {
            address: Ipv4Addr::new(octets[0], octets[1], octets[2], 40),
            export: NFS_EXPORT.to_owned(),
            proxy_port,
        }
    }
}

fn user_data(settings: &LabSettings, server: &LabNfsServer) -> String {
    let octets = settings.gateway.octets();
    let clients = format!("{}.{}.{}.0/{}", octets[0], octets[1], octets[2], settings.prefix_length);
    format!(
        "#cloud-config\npackages:\n  - nfs-kernel-server\nwrite_files:\n  - path: /etc/exports\n    content: \"{export} {clients}(rw,sync,no_root_squash,no_subtree_check)\\n\"\nruncmd:\n  - mkdir -p {export}\n  - chmod 0755 {export}\n  - exportfs -ra\n  - systemctl restart nfs-kernel-server\n",
        export = server.export,
    )
}

fn instance_definition(settings: &LabSettings, server: &LabNfsServer) -> serde_json::Value {
    json!({
        "name": NFS_INSTANCE,
        "type": "virtual-machine",
        "source": { "type": "image", "mode": "pull", "server": IMAGE_SERVER, "protocol": "simplestreams", "alias": IMAGE_ALIAS },
        "config": {
            "limits.cpu": "1",
            "limits.memory": "1GiB",
            "cloud-init.user-data": user_data(settings, server),
        },
        "devices": {
            "root": { "type": "disk", "path": "/", "pool": settings.storage_pool, "size": "10GiB" },
            "eth0": { "type": "nic", "network": settings.network, "ipv4.address": server.address.to_string() },
            "nfs": {
                "type": "proxy",
                "listen": format!("tcp:{}:{}", settings.host_address, server.proxy_port),
                "connect": format!("tcp:{}:{NFS_PORT}", server.address),
                "nat": "true",
            },
        },
    })
}

pub async fn create(lxd: &LxdClient, settings: &LabSettings, server: &LabNfsServer) -> anyhow::Result<()> {
    lxd.request(Method::POST, "/1.0/instances", Some(instance_definition(settings, server))).await?;
    lxd.change_state(NFS_INSTANCE, "start", false).await
}

pub async fn wait_until_serving(settings: &LabSettings, server: &LabNfsServer) -> anyhow::Result<Duration> {
    let started = Instant::now();
    let address = SocketAddr::from((settings.host_address, server.proxy_port));
    loop {
        let connected = tokio::time::timeout(READY_POLL, tokio::net::TcpStream::connect(address)).await;
        if matches!(connected, Ok(Ok(_))) {
            return Ok(started.elapsed());
        }
        if started.elapsed() >= READY_TIMEOUT {
            bail!("the NFS server did not start serving within {} seconds", READY_TIMEOUT.as_secs());
        }
        tokio::time::sleep(READY_POLL).await;
    }
}
