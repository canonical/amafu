use std::path::Path;
use std::time::Duration;

use amafu_system::boot_media::{BOOT_PARTITION_NAME, DATA_PARTITION_NAME, find_partition};
use amafu_system::early_boot::{mount_boot_partition, mount_data_partition, set_hostname};
use amafu_system::modules::load_listed_modules;
use amafu_system::mount_essential_filesystems;

use amafu_node::configuration::NodeConfiguration;

const MODULES_DIRECTORY: &str = "/usr/lib/amafu/modules";
const MODULE_LIST: &str = "/usr/lib/amafu/modules/load-order";
const SYSFS_BLOCK: &str = "/sys/class/block";
const BOOT_MOUNT: &str = "/run/amafu/boot";
const CONFIGURATION_FILE: &str = "/run/amafu/boot/amafu/node-configuration.yaml";
const RESOLVER_FILE: &str = "/etc/resolv.conf";
const PARTITION_WAIT: Duration = Duration::from_secs(10);
const PARTITION_POLL: Duration = Duration::from_millis(100);

async fn wait_for_partition(name: &str) -> anyhow::Result<std::path::PathBuf> {
    let started = std::time::Instant::now();
    loop {
        if let Some(device) = find_partition(Path::new(SYSFS_BLOCK), name) {
            return Ok(device);
        }
        if started.elapsed() >= PARTITION_WAIT {
            anyhow::bail!("no partition named {name} appeared");
        }
        tokio::time::sleep(PARTITION_POLL).await;
    }
}

fn write_name_resolution(resolvers: &[std::net::Ipv4Addr]) -> anyhow::Result<()> {
    if resolvers.is_empty() {
        return Ok(());
    }
    let lines: Vec<String> = resolvers.iter().map(|resolver| format!("nameserver {resolver}")).collect();
    std::fs::write(RESOLVER_FILE, lines.join("\n") + "\n")?;
    Ok(())
}

pub async fn early_boot() -> anyhow::Result<NodeConfiguration> {
    mount_essential_filesystems()?;
    let loaded = load_listed_modules(Path::new(MODULES_DIRECTORY), Path::new(MODULE_LIST))?;
    tracing::info!(loaded, "kernel modules loaded");
    let boot_partition = wait_for_partition(BOOT_PARTITION_NAME).await?;
    mount_boot_partition(&boot_partition, Path::new(BOOT_MOUNT))?;
    let configuration = NodeConfiguration::load(Path::new(CONFIGURATION_FILE))?;
    set_hostname(configuration.node.name.as_str())?;
    let data_partition = wait_for_partition(DATA_PARTITION_NAME).await?;
    mount_data_partition(&data_partition, &configuration.directories.data)?;
    if let Some(underlay) = &configuration.network {
        amafu_network::configure_underlay(underlay).await?;
    }
    write_name_resolution(&configuration.compute.resolvers)?;
    Ok(configuration)
}
