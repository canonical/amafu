# 0010: The boot partition holds what the node reads on demand

Status: accepted for milestone 2

## Context

GRUB reads the kernel and the initramfs from the FAT boot partition before the kernel starts, and the kernel then
unpacks the initramfs into memory. Milestone 2 added nftables, erofs-utils (with its libraries), firmware, a guest
kernel, the container initramfs and more kernel modules. Kept in the initramfs, they pushed the node past its
20-second boot budget.

## Decision

- The initramfs keeps only what the node needs before the boot partition is mounted: `amafu-node`, its libraries,
  the NLS module the FAT mount needs, and (in test images) the test guest.
- The boot partition (`/run/amafu/boot/amafu`) holds the guest `vmlinux`, `CLOUDHV.fd`, the container initramfs, the
  kernel modules and `tools/` (`nft`, `mkfs.erofs` and the libraries they need, run with `LD_LIBRARY_PATH`). It is
  mounted read-only, `nosuid` and `nodev`, and executable so the tools can run.
- Kernel modules load in the background while the state store starts, and the compute roles wait for them.
- The guest kernel, the container initramfs and the firmware are read once in the background at boot, so the first
  VM on a node does not wait for them: a cold guest kernel took 3.8 seconds to load in the lab, and 2.4 milliseconds
  from the page cache.

## Consequences

- The initramfs is 8.6 MB instead of 23 MB. In the lab a node serves its API about 17 seconds after a restart, and
  about 20 seconds after the very first boot of a new lab, whose extra time is in kernel hardware setup and the
  first start of a fresh LXD volume.
- The tools come from the build machine's packages (`nftables`, `erofs-utils`), so the build must run on the Ubuntu
  release the image targets.
- The boot partition is not covered by Secure Boot any more than the initramfs is; both are trusted because the
  signed kernel and GRUB load them from the node's own disk.
