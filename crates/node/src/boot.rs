use std::path::Path;
use std::time::Duration;

use amafu_boot_slots::EspLayout;
use amafu_boot_slots::layout::booted_slot;
use amafu_system::boot_media::{BOOT_PARTITION_NAME, DATA_PARTITION_NAME, find_partition};
use amafu_system::early_boot::{expose_boot_slot, mount_boot_partition, mount_data_partition, set_hostname};
use amafu_system::files::read_into_page_cache;
use amafu_system::modules::load_listed_modules;
use amafu_system::mount_essential_filesystems;

use amafu_node::configuration::NodeConfiguration;

const MODULES_DIRECTORY: &str = "/usr/lib/amafu/modules";
const BOOT_MODULES_DIRECTORY: &str = "/run/amafu/boot/amafu/modules";
const MODULE_LIST: &str = "/run/amafu/boot/amafu/modules/load-order";
const EARLY_MODULE_LIST: &str = "/usr/lib/amafu/modules/early-load-order";
const SYSFS_BLOCK: &str = "/sys/class/block";
pub const ESP_MOUNT: &str = "/run/amafu/esp";
const BOOTED_SLOT_MOUNT: &str = "/run/amafu/boot/amafu";
const KERNEL_COMMAND_LINE: &str = "/proc/cmdline";
pub const CONFIGURATION_FILE: &str = "/run/amafu/esp/amafu/node-configuration.yaml";
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

#[must_use]
pub fn booted_from() -> amafu_domain::kinds::node::BootSlot {
    booted_slot(&std::fs::read_to_string(KERNEL_COMMAND_LINE).unwrap_or_default())
}

pub type ModuleLoading = tokio::task::JoinHandle<()>;

pub fn boot_modules_directory() -> std::path::PathBuf {
    std::path::PathBuf::from(BOOT_MODULES_DIRECTORY)
}

fn load_modules_in_background() -> ModuleLoading {
    tokio::task::spawn_blocking(|| {
        match load_listed_modules(Path::new(BOOT_MODULES_DIRECTORY), Path::new(MODULE_LIST)) {
            Ok(loaded) => tracing::info!(loaded, "kernel modules loaded"),
            Err(error) => tracing::error!(%error, "kernel modules cannot be loaded"),
        }
    })
}

fn warm_guest_boot_files(configuration: &NodeConfiguration) {
    let compute = &configuration.compute;
    let files =
        [compute.container_kernel.clone(), compute.container_initramfs.clone(), compute.firmware.clone()];
    tokio::task::spawn_blocking(move || {
        for file in files {
            if let Err(error) = read_into_page_cache(&file) {
                tracing::warn!(file = %file.display(), %error, "a guest boot file cannot be read ahead");
            }
        }
    });
}

pub async fn early_boot() -> anyhow::Result<(NodeConfiguration, ModuleLoading)> {
    mount_essential_filesystems()?;
    load_listed_modules(Path::new(MODULES_DIRECTORY), Path::new(EARLY_MODULE_LIST))?;
    let boot_partition = wait_for_partition(BOOT_PARTITION_NAME).await?;
    mount_boot_partition(&boot_partition, Path::new(ESP_MOUNT))?;
    let slot = booted_slot(&std::fs::read_to_string(KERNEL_COMMAND_LINE)?);
    expose_boot_slot(&EspLayout::new(ESP_MOUNT).slot_directory(slot), Path::new(BOOTED_SLOT_MOUNT))?;
    tracing::info!(slot = slot.letter(), "booted from slot");
    let modules = load_modules_in_background();
    let configuration = NodeConfiguration::load(Path::new(CONFIGURATION_FILE))?;
    set_hostname(configuration.node.name.as_str())?;
    warm_guest_boot_files(&configuration);
    let data_partition = wait_for_partition(DATA_PARTITION_NAME).await?;
    mount_data_partition(&data_partition, &configuration.directories.data)?;
    for interface in [&configuration.management, &configuration.network].into_iter().flatten() {
        amafu_network::configure_underlay(interface).await?;
    }
    write_name_resolution(&configuration.compute.resolvers)?;
    Ok((configuration, modules))
}
