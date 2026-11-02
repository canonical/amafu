# 0003: Boot the lab through the distribution's signed chain, with the whole system in the initramfs

Status: accepted for milestone 1; enrolling Amafu's own Secure Boot keys is a hardware qualification item

## Context

The plan wants the test image to boot in the virtual lab with Secure Boot and a virtual TPM, its root file system
verified, within 20 seconds. Its week-0 assumptions table names the fallback if LXD VMs cannot use our own keys:
test verified boot with dm-verity and measured boot through the virtual TPM, and record key enrolment as a
qualification item.

What LXD 6.10 offers, checked on the lab:

- `boot.mode=uefi-secureboot` (the default) boots OVMF with Microsoft's keys enrolled. There is no API to enroll
  other keys, and the guest cannot replace them without the platform key.
- `boot.mode=uefi-nosecureboot` boots a firmware build without Secure Boot support: it has no `SetupMode`,
  `SecureBoot` or `PK` variables at all, so keys cannot be enrolled from setup mode either.

So an image signed with an Amafu key cannot boot with Secure Boot enforced in the lab.

## Decision

- The node boots through Ubuntu's signed chain: Microsoft-signed `shim`, Canonical-signed GRUB, and the
  Canonical-signed Ubuntu kernel (7.0.0-42), all taken unmodified from the archive. Secure Boot is enforced on every
  stage, and the kernel runs in lockdown mode, so only Canonical-signed modules load. The node reports
  `SecureBoot=1` through the test-only diagnostic channel, and the rig checks it on every `lab up`.
- The whole operating system is the initramfs: `amafu-node` as `/init`, the C libraries it links, the kernel modules
  it loads, and in test builds the test guest. There is no separate root file system to verify, so dm-verity is
  not needed yet. The initramfs is not covered by a signature in this chain. GRUB measures it into the virtual TPM,
  which is where sealed secrets will be bound (milestone 3).
- Persistent state lives on an ext4 `amafu-data` partition. The node configuration is a file on the `amafu-boot`
  partition, written per lab by `image/assemble-disk`, so the boot files are identical across nodes and builds.
- The image is 28 MiB. A node VM serves its API about 19 seconds after LXD starts it: about 8.5 s of firmware,
  5–7 s for shim and GRUB to read the kernel and initramfs, about 4.5 s of kernel start, and 1 s of Amafu.

## Consequences

- Before release, the boot path must become: a unified kernel image signed with Amafu's key (kernel, initramfs and
  command line covered by one signature), enrolled through MAAS or the server's firmware, with A/B slots
  (milestone 5). This is the qualification item the plan anticipates.
- Until then, an attacker who can write the boot partition can change the initramfs. Measured boot detects that, but
  nothing yet depends on the measurement.
- The release image (`image/build release`) differs from the test image only in the diagnostic channel and the test
  guest. The check that diffs them is not written yet.
- The boot-time budget is met with 1 s to spare. Firmware time dominates and is outside our control in the lab;
  the controllable part is the size GRUB reads, so the test guest uses the node's own kernel instead of a second
  copy, and the release profile strips symbols.
