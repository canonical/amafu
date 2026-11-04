use std::net::Ipv4Addr;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::lxd::LxdClient;
use crate::machines::{MachineSize, MachineSource, create as create_machine, definition, wait_for_marker};
use crate::node_images::{IMAGE_DIRECTORY, IMAGE_SERVER_PORT};
use crate::topology::{LabSettings, LabTopology};

pub const NFS_EXPORT: &str = "/srv/amafu";
const READY_MARKER: &str = "/var/lib/amafu-nfs-ready";
const READY_TIMEOUT: Duration = Duration::from_secs(900);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabNfsServer {
    pub address: Ipv4Addr,
    pub export: String,
}

fn user_data(settings: &LabSettings) -> String {
    format!(
        "#cloud-config\npackages:\n  - nfs-kernel-server\nwrite_files:\n  - path: /etc/exports\n    content: \"{NFS_EXPORT} {}(rw,sync,no_root_squash,no_subtree_check)\\n\"\n  - path: /etc/systemd/system/amafu-images.service\n    content: |\n      [Service]\n      ExecStart=/usr/bin/python3 -m http.server {IMAGE_SERVER_PORT} --directory {IMAGE_DIRECTORY}\n      Restart=always\n      [Install]\n      WantedBy=multi-user.target\nruncmd:\n  - [mkdir, -p, {NFS_EXPORT}, {IMAGE_DIRECTORY}]\n  - [chmod, \"0755\", {NFS_EXPORT}]\n  - [exportfs, -ra]\n  - [systemctl, restart, nfs-kernel-server]\n  - [systemctl, enable, --now, amafu-images.service]\n  - [touch, {READY_MARKER}]\n",
        settings.underlay
    )
}

pub async fn create(
    lxd: &LxdClient,
    settings: &LabSettings,
    topology: &LabTopology,
) -> anyhow::Result<LabNfsServer> {
    let machine = &topology.nfs_server;
    let size = MachineSize { cpu_count: 1, memory_mebibytes: 768, disk_gibibytes: 10 };
    create_machine(lxd, definition(settings, machine, &size, &MachineSource::Ubuntu, &user_data(settings)))
        .await?;
    Ok(LabNfsServer { address: machine.underlay_address, export: NFS_EXPORT.to_owned() })
}

pub async fn wait_until_serving(lxd: &LxdClient, topology: &LabTopology) -> anyhow::Result<Duration> {
    wait_for_marker(lxd, &topology.nfs_server.name, READY_MARKER, READY_TIMEOUT).await
}
