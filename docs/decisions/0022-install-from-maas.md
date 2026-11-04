# 0022: Install from MAAS through an in-memory Ubuntu, with a local journal

Status: accepted for milestone 5, to be confirmed in the MAAS lab

## Decision

- **`amafu install` runs an `install` operation on the operator's machine.** Its steps are: verify the image, deploy
  each machine (core members first), wait until each node runs the image confirmed, and record the image as the
  cluster's. The operation lives in a journal file in the state directory, written atomically after every change,
  so the installer resumes where it stopped after a crash between any two steps; the runner and step contract are
  those of the cluster (0017).
- **The install description** names the image manifest, the boot loader bundle (shim, GRUB and `grub.cfg`), the
  trusted image keys, the MAAS endpoint and key, and each machine's hostname, node name, roles, zone and addresses.
- **The installer is the cluster's first certificate authority.** It creates the authority and an admin token once,
  keeps them in the state directory readable only by its owner, and issues each node's certificate into the node
  configuration it deploys. This replaces the rig issuing certificates (0005).
- **MAAS deploys Ubuntu in memory** (`ephemeral_deploy`) with user data that downloads the image and boot loader,
  checks their digests, partitions the disk into `amafu-boot` and `amafu-data`, writes slot `a`, the manifest, the
  node configuration and `grubenv`, puts an `Amafu` boot entry first and restarts. The disk is not in use while it is
  written, and MAAS marks the machine deployed when cloud-init finishes.
- **The MAAS client** signs requests with OAuth 1.0 PLAINTEXT from the three-part API key and maps MAAS statuses to
  ready, allocated, deploying, deployed and failed.

## Consequences

- Node configurations, including private keys, pass through MAAS user data. A join token exchanged for a certificate
  at first boot, bound to the TPM, replaces this (0005).
- A machine whose firmware puts network boot back in front of the `Amafu` entry would boot the in-memory Ubuntu
  again; the MAAS lab checks the boot order MAAS composes.
