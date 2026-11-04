use amafu_domain::node_image::NodeImage;
use base64::Engine;

use crate::description::BootLoader;

const SCRIPT: &str = r#"#!/bin/bash
set -euo pipefail
exec > >(tee /dev/console) 2>&1
echo "amafu installer: writing ${AMAFU_NODE}"
work="$(mktemp -d)"
fetch() {
    curl --fail --silent --show-error --location --retry 5 --output "$2" "$1"
    echo "$3  $2" | sha256sum --check --quiet
}
fetch "${AMAFU_IMAGE_URL}" "${work}/node-image.tar" "${AMAFU_IMAGE_DIGEST}"
fetch "${AMAFU_BOOT_LOADER_URL}" "${work}/boot-loader.tar" "${AMAFU_BOOT_LOADER_DIGEST}"
disk="$(lsblk --nodeps --paths --noheadings --output NAME,TYPE | awk '$2 == "disk" { print $1; exit }')"
echo "amafu installer: writing ${disk}"
wipefs --all --force "${disk}"
sgdisk --zap-all "${disk}"
sgdisk --new=1:1MiB:+256MiB --typecode=1:ef00 --change-name=1:amafu-boot \
    --new=2:0:0 --typecode=2:8300 --change-name=2:amafu-data "${disk}"
partprobe "${disk}"
udevadm settle
boot_partition="$(lsblk --paths --noheadings --list --output NAME,PARTLABEL "${disk}" | awk '$2 == "amafu-boot" { print $1 }')"
data_partition="$(lsblk --paths --noheadings --list --output NAME,PARTLABEL "${disk}" | awk '$2 == "amafu-data" { print $1 }')"
mkfs.vfat -F 32 -n AMAFU-BOOT "${boot_partition}"
mkfs.ext4 -q -F -L amafu-data "${data_partition}"
mkdir -p "${work}/esp"
mount "${boot_partition}" "${work}/esp"
tar --extract --no-same-owner --file "${work}/boot-loader.tar" --directory "${work}/esp"
mkdir -p "${work}/esp/amafu-a" "${work}/esp/amafu"
tar --extract --no-same-owner --file "${work}/node-image.tar" --directory "${work}/esp/amafu-a"
echo "${AMAFU_MANIFEST}" | base64 --decode > "${work}/esp/amafu-a/node-image.json"
echo "${AMAFU_CONFIGURATION}" | base64 --decode > "${work}/esp/amafu/node-configuration.yaml"
printf '# GRUB Environment Block\ndefault_slot=a\nnext_slot=\n' > "${work}/esp/amafu/grubenv"
head -c "$((1024 - $(stat --format=%s "${work}/esp/amafu/grubenv")))" /dev/zero | tr '\0' '#' >> "${work}/esp/amafu/grubenv"
umount "${work}/esp"
sync
put_amafu_first() {
    for entry in $(efibootmgr | awk '/ Amafu$/ { print substr($1, 5, 4) }'); do
        efibootmgr --quiet --delete-bootnum --bootnum "${entry}"
    done
    efibootmgr --quiet --create --disk "${disk}" --part 1 --label Amafu --loader '\EFI\BOOT\BOOTX64.EFI'
    amafu_entry="$(efibootmgr | awk '/ Amafu$/ { print substr($1, 5, 4); exit }')"
    others="$(efibootmgr | awk -F': ' '/^BootOrder/ { print $2 }' | tr ',' '\n' | grep -v "^${amafu_entry}$" | paste -sd, -)"
    efibootmgr --quiet --bootorder "${amafu_entry}${others:+,${others}}"
}
put_amafu_first || echo "amafu installer: the firmware boot order could not be changed; the disk must come before the network"
echo "amafu installer: ${AMAFU_NODE} is written; restarting into it"
systemd-run --on-active=15 --unit amafu-restart systemctl reboot
"#;

fn encoded(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn shell_word(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[must_use]
pub fn installer_script(
    node: &str,
    image: &NodeImage,
    boot_loader: &BootLoader,
    manifest: &[u8],
    configuration: &[u8],
) -> String {
    let variables = [
        ("AMAFU_NODE", node.to_owned()),
        ("AMAFU_IMAGE_URL", image.url.clone()),
        ("AMAFU_IMAGE_DIGEST", image.digest.hexadecimal().to_owned()),
        ("AMAFU_BOOT_LOADER_URL", boot_loader.url.clone()),
        ("AMAFU_BOOT_LOADER_DIGEST", boot_loader.digest.hexadecimal().to_owned()),
        ("AMAFU_MANIFEST", encoded(manifest)),
        ("AMAFU_CONFIGURATION", encoded(configuration)),
    ];
    let assignments: Vec<String> =
        variables.iter().map(|(name, value)| format!("{name}={}", shell_word(value))).collect();
    let assignments = assignments.join("\n") + "\n";
    let (shebang, body) = SCRIPT.split_once('\n').unwrap_or((SCRIPT, ""));
    format!("{shebang}\n{assignments}{body}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_quoted_so_the_shell_cannot_split_them() {
        assert_eq!(shell_word("it's"), "'it'\\''s'");
        let image = NodeImage {
            version: String::from("0.2.0"),
            feature_level: amafu_domain::features::FeatureLevel(1),
            digest: "a".repeat(64).parse().unwrap(),
            size_bytes: 1,
            url: String::from("http://images.lab/node-image.tar"),
            signature: "0".repeat(128),
        };
        let boot_loader = BootLoader {
            url: String::from("http://images.lab/boot-loader.tar"),
            digest: "b".repeat(64).parse().unwrap(),
        };
        let script = installer_script("node-1", &image, &boot_loader, b"{}", b"version: 1\n");
        assert!(script.starts_with("#!/bin/bash\nAMAFU_NODE='node-1'\n"));
        assert!(script.contains(&format!("AMAFU_IMAGE_DIGEST='{}'", "a".repeat(64))));
        assert!(script.contains("AMAFU_CONFIGURATION='dmVyc2lvbjogMQo='"));
    }
}
