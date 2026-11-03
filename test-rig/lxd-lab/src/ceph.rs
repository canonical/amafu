use std::net::SocketAddrV4;
use std::time::Duration;

use amafu_domain::kinds::storage_backend::StorageProviderSpecification;

use crate::lxd::LxdClient;
use crate::machines::{
    MachineSize, MachineSource, create as create_machine, definition, read_file, wait_for_outcome,
};
use crate::topology::{LabSettings, LabTopology};

const READY_MARKER: &str = "/var/lib/amafu-ceph-ready";
const FAILED_MARKER: &str = "/var/lib/amafu-ceph-failed";
const SETUP_LOG: &str = "/var/log/amafu-ceph-setup.log";
const KEY_FILE: &str = "/root/amafu-client.key";
const READY_TIMEOUT: Duration = Duration::from_secs(1800);
const MONITOR_PORT: u16 = 6789;
const DASHBOARD_PORT: u16 = 8080;
const POOL: &str = "amafu";
const CLIENT: &str = "amafu";
const DASHBOARD_USER: &str = "amafu";
const CHANNEL: &str = "squid/stable";

fn user_data(settings: &LabSettings, address: &str, password: &str) -> String {
    format!(
        r#"#cloud-config
write_files:
  - path: /usr/local/bin/amafu-ceph-setup
    permissions: "0755"
    content: |
      #!/bin/bash
      set -euxo pipefail
      snap install microceph --channel={CHANNEL}
      snap refresh --hold microceph
      microceph cluster bootstrap --public-network {network} --cluster-network {network} --microceph-ip {address}
      microceph disk add loop,4G,3
      ceph=microceph.ceph
      $ceph config set osd osd_memory_target 939524096
      $ceph config set global mon_allow_pool_size_one true
      $ceph osd pool create {POOL} 32
      $ceph osd pool set {POOL} size 1 --yes-i-really-mean-it
      microceph.rbd pool init {POOL}
      $ceph auth get-or-create client.{CLIENT} mon 'profile rbd' osd 'profile rbd pool={POOL}' > /dev/null
      $ceph auth get-key client.{CLIENT} > {KEY_FILE}
      $ceph mgr module enable dashboard
      $ceph config set mgr mgr/dashboard/ssl false
      $ceph config set mgr mgr/dashboard/server_addr {address}
      $ceph config set mgr mgr/dashboard/server_port {DASHBOARD_PORT}
      printf '%s' '{password}' > /root/dashboard-password
      $ceph dashboard ac-user-create {DASHBOARD_USER} -i /root/dashboard-password administrator
      $ceph mgr module disable dashboard
      $ceph mgr module enable dashboard
      touch {READY_MARKER}
runcmd:
  - [sh, -c, "/usr/local/bin/amafu-ceph-setup > {SETUP_LOG} 2>&1 || touch {FAILED_MARKER}"]
"#,
        network = settings.underlay,
    )
}

pub async fn create(
    lxd: &LxdClient,
    settings: &LabSettings,
    topology: &LabTopology,
    password: &str,
) -> anyhow::Result<()> {
    let machine = &topology.ceph;
    let size = MachineSize { cpu_count: 2, memory_mebibytes: 4096, disk_gibibytes: 30 };
    let user_data = user_data(settings, &machine.underlay_address.to_string(), password);
    create_machine(lxd, definition(settings, machine, &size, &MachineSource::Ubuntu, &user_data)).await
}

pub async fn wait_until_serving(
    lxd: &LxdClient,
    topology: &LabTopology,
    password: &str,
) -> anyhow::Result<StorageProviderSpecification> {
    let machine = &topology.ceph;
    wait_for_outcome(lxd, &machine.name, READY_MARKER, FAILED_MARKER, SETUP_LOG, READY_TIMEOUT).await?;
    let key = read_file(lxd, &machine.name, KEY_FILE).await?.trim().to_owned();
    Ok(StorageProviderSpecification::CephRbd {
        monitors: vec![SocketAddrV4::new(machine.underlay_address, MONITOR_PORT)],
        pool: POOL.to_owned(),
        user: CLIENT.to_owned(),
        key,
        management: format!("http://{}:{DASHBOARD_PORT}", machine.underlay_address),
        management_user: DASHBOARD_USER.to_owned(),
        management_password: password.to_owned(),
    })
}
