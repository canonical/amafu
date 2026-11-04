# 0021: Two boot slots on the ESP, chosen through grubenv, and upgrades as operations

Status: accepted for milestone 5

## Context

The plan asks for A/B slots selected for one boot by the UEFI `BootNext` variable, a new slot made the default only
after the node has stayed healthy for two minutes, and upgrades and rollbacks built as operations from steps that
already exist. It also asks that rolling back is refused once the cluster is finalized.

What the lab and MAAS allow, checked while designing:

- The node boots through shim and Canonical's signed GRUB (0003), whose configuration lives at a fixed path on the
  ESP. Ubuntu's GRUB reads and writes a GRUB environment block (`load_env`, `save_env`) under Secure Boot.
- MAAS machines are commonly set to network boot first, and MAAS answers a deployed machine with a local-boot
  configuration that chainloads the disk's loader, which ignores a per-slot boot entry.

## Decision

- **The ESP holds two slot directories, `amafu-a` and `amafu-b`, and one shared `amafu` directory** with the node
  configuration and `grubenv`. A slot holds the kernel, initramfs, guest kernel, firmware, modules, tools and
  `node-image.json`, the signed manifest of the image it holds. The manifest is written last, so a slot without one is
  empty.
- **GRUB picks the slot from `grubenv`:** `next_slot` if set, which it clears with `save_env` before booting it, and
  `default_slot` otherwise. The kernel command line carries `amafu.slot=`. A new slot is tried once, like `BootNext`,
  but the choice works whatever the firmware boot order and whoever chainloads GRUB. This replaces the plan's
  `BootNext`.
- **Early boot mounts the ESP writable but not executable at `/run/amafu/esp`** and bind-mounts the booted slot
  read-only and executable at `/run/amafu/boot/amafu`, the path every other part of the node already used.
- **A node image is a tar of one slot**, identified by its SHA-256 and size and signed with Ed25519 over its version,
  feature level, digest and size. The URL is not signed, so a mirror can serve it. Nodes and the core trust the keys
  in the node configuration's `upgrade.image_keys`; `amafu release create-key` and `amafu release sign` make them.
- **The boot-slots controller on each node** writes a requested image into the inactive slot after checking its
  signature, size and digest, boots it once when asked and no VM runs on the node, makes it the default after
  `confirm_after_seconds` of holding its lease while ready, and reboots into the default if that never happens within
  `give_up_after_seconds`. A boot that ends in the other slot than the one being tried is reported as
  `fallen_back_from`, kept on the data partition, and the image is not tried again. A refused image is reported too.
- **Before rebooting, the node surrenders its lease.** The lease moves to a new epoch marked lost and released, so
  the node's old epoch is fenced, node-health starts no recovery, and the node takes a fresh lease when it is back.
  Only init may reboot (0020).
- **Upgrades are operations** whose steps come from a plan stored in the operation: verify the image, record it as
  the cluster's image, then for each node evacuate, write the inactive slot, reboot into it, wait until it runs the
  image confirmed, and uncordon, with a soak after each wave. The waves are a canary, then each zone, then the core
  voters one at a time. A rollback is the same plan with the previous image and the waves reversed.
- **These steps may be abandoned.** A permanent failure (a node fell back, refused the image or never became
  healthy) ends the upgrade `Failed`, and a cancellation ends it `Cancelled` at the next step, without undoing the
  nodes already upgraded; a mixed cluster is a supported state, and the operator rolls back or retries.
- **`ClusterVersion` is an admin-only singleton** holding the image, the previous image and the finalized feature
  level. `amafu upgrade finalize` raises the level to the image's, which is what enables a `Feature`, and forgets the
  previous image, so rollback has nothing to go back to and `verify-image` refuses any image below the level.

## Consequences

- An image built for this layout cannot be installed over a disk from milestone 4; the lab is rebuilt.
- The two features named by the plan are gates with nothing behind them yet; each is wired when its behaviour lands.
- A node that cannot boot at all and has no watchdog armed (a hang before PID 1 starts) relies on the kernel's
  `panic=10`; a hardware watchdog on real nodes covers the rest.
