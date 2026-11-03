use std::net::SocketAddrV4;
use std::time::Duration;

use amafu_domain::kinds::storage_backend::StorageProviderSpecification;
use reqwest::Method;
use serde_json::json;

use crate::lxd::LxdClient;
use crate::machines::{MachineSize, MachineSource, create as create_machine, definition, wait_for_marker};
use crate::topology::{LabSettings, LabTopology};

pub const SPDK_VERSION: &str = "v24.09";
pub const IMAGE_ALIAS: &str = "amafu-spdk-target-v24-09";
const BUILD_INSTANCE: &str = "nvme-target-build";
const BUILT_MARKER: &str = "/var/lib/amafu-spdk-built";
const READY_MARKER: &str = "/run/amafu-nvme-target-ready";
const BUILD_TIMEOUT: Duration = Duration::from_secs(3600);
const READY_TIMEOUT: Duration = Duration::from_secs(600);
const MANAGEMENT_PORT: u16 = 9009;
const TARGET_PORT: u16 = 4420;
const MANAGEMENT_USER: &str = "amafu";
const POOL: &str = "amafu";

fn build_user_data() -> String {
    format!(
        r#"#cloud-config
write_files:
  - path: /usr/local/bin/amafu-build-spdk
    permissions: "0755"
    content: |
      #!/bin/bash
      set -euxo pipefail
      export DEBIAN_FRONTEND=noninteractive PIP_BREAK_SYSTEM_PACKAGES=1
      apt-get update
      apt-get install -y git
      git clone --depth 1 --branch {SPDK_VERSION} https://github.com/spdk/spdk /opt/spdk
      cd /opt/spdk
      git submodule update --init --depth 1
      scripts/pkgdep.sh
      ./configure --disable-tests --disable-unit-tests --disable-examples --without-crypto --without-vhost --without-virtio
      make -j"$(nproc)"
      apt-get clean
      touch {BUILT_MARKER}
runcmd:
  - [sh, -c, "/usr/local/bin/amafu-build-spdk > /var/log/amafu-build-spdk.log 2>&1"]
"#
    )
}

fn run_user_data(address: &str, password: &str) -> String {
    format!(
        r#"#cloud-config
write_files:
  - path: /usr/local/bin/amafu-nvme-target-setup
    permissions: "0755"
    content: |
      #!/bin/bash
      set -ux
      rpc=/opt/spdk/scripts/rpc.py
      for attempt in $(seq 120); do $rpc spdk_get_version && break; sleep 1; done
      $rpc nvmf_create_transport -t TCP
      mkdir -p /var/lib/spdk
      [ -e /var/lib/spdk/pool.img ] || truncate -s 8G /var/lib/spdk/pool.img
      $rpc bdev_aio_create /var/lib/spdk/pool.img pool 4096
      $rpc bdev_lvol_get_lvstores -l {POOL} || $rpc bdev_lvol_create_lvstore pool {POOL}
      touch {READY_MARKER}
  - path: /etc/systemd/system/amafu-nvme-target.service
    content: |
      [Unit]
      After=network-online.target
      [Service]
      ExecStartPre=/usr/sbin/sysctl -w vm.nr_hugepages=600
      ExecStart=/opt/spdk/build/bin/nvmf_tgt -m 0x1 -s 1024
      ExecStartPost=/usr/local/bin/amafu-nvme-target-setup
      Restart=always
      [Install]
      WantedBy=multi-user.target
  - path: /etc/systemd/system/amafu-nvme-management.service
    content: |
      [Unit]
      After=amafu-nvme-target.service
      [Service]
      ExecStart=/usr/bin/python3 /opt/spdk/scripts/rpc_http_proxy.py {address} {MANAGEMENT_PORT} {MANAGEMENT_USER} {password}
      Restart=always
      [Install]
      WantedBy=multi-user.target
runcmd:
  - [systemctl, daemon-reload]
  - [systemctl, enable, --now, amafu-nvme-target.service, amafu-nvme-management.service]
"#
    )
}

async fn image_exists(lxd: &LxdClient) -> anyhow::Result<bool> {
    lxd.exists(&format!("/1.0/images/aliases/{IMAGE_ALIAS}")).await
}

async fn build_image(lxd: &LxdClient, settings: &LabSettings, topology: &LabTopology) -> anyhow::Result<()> {
    let mut machine = topology.nvme_target.clone();
    machine.name = String::from(BUILD_INSTANCE);
    let size = MachineSize { cpu_count: 4, memory_mebibytes: 4096, disk_gibibytes: 20 };
    tracing::info!("building the SPDK target image once; this takes a while");
    create_machine(lxd, definition(settings, &machine, &size, &MachineSource::Ubuntu, &build_user_data()))
        .await?;
    wait_for_marker(lxd, BUILD_INSTANCE, BUILT_MARKER, BUILD_TIMEOUT).await?;
    lxd.execute(BUILD_INSTANCE, "cloud-init clean --logs").await?;
    lxd.change_state(BUILD_INSTANCE, "stop", true).await?;
    let publication = json!({
        "source": {"type": "instance", "name": BUILD_INSTANCE},
        "aliases": [{"name": IMAGE_ALIAS, "description": "SPDK NVMe-oF target for the Amafu virtual lab"}],
        "properties": {"description": format!("SPDK {SPDK_VERSION} on Ubuntu 24.04")},
    });
    lxd.request(Method::POST, "/1.0/images", Some(publication)).await?;
    lxd.request(Method::DELETE, &format!("/1.0/instances/{BUILD_INSTANCE}"), None).await?;
    Ok(())
}

pub async fn create(
    lxd: &LxdClient,
    settings: &LabSettings,
    topology: &LabTopology,
    password: &str,
) -> anyhow::Result<StorageProviderSpecification> {
    if !image_exists(lxd).await? {
        build_image(lxd, settings, topology).await?;
    }
    let machine = &topology.nvme_target;
    let size = MachineSize { cpu_count: 2, memory_mebibytes: 2560, disk_gibibytes: 20 };
    let user_data = run_user_data(&machine.underlay_address.to_string(), password);
    create_machine(
        lxd,
        definition(settings, machine, &size, &MachineSource::Image(IMAGE_ALIAS.to_owned()), &user_data),
    )
    .await?;
    Ok(StorageProviderSpecification::NvmeTcp {
        management: format!("http://{}:{MANAGEMENT_PORT}/", machine.underlay_address),
        management_user: Some(MANAGEMENT_USER.to_owned()),
        management_password: Some(password.to_owned()),
        addresses: vec![SocketAddrV4::new(machine.underlay_address, TARGET_PORT)],
        pool: POOL.to_owned(),
    })
}

pub async fn wait_until_serving(lxd: &LxdClient, topology: &LabTopology) -> anyhow::Result<Duration> {
    wait_for_marker(lxd, &topology.nvme_target.name, READY_MARKER, READY_TIMEOUT).await
}
