# 0001: Embed Cloud Hypervisor's vmm crate in a per-VM process

Status: accepted for milestone 1

## Context

The plan has the node run each VM in its own sandboxed process and lists "Cloud Hypervisor's internal crates are
not a stable library API" as a high-likelihood risk, with running the Cloud Hypervisor binary over its HTTP socket
as the fallback. The spike embedded the `vmm` crate of Cloud Hypervisor v54.0 (commit
`038ce5a72d761380a02f714a4eb4ca63e0ea073d`).

## What the spike found

- `vmm` builds as a git dependency with the `kvm` feature and no others. It is a large dependency (about 300
  crates), but pure Rust: no C library is needed.
- Our lockfile resolved `vfio_user` 0.1.7, which pulls a second, incompatible `vfio-bindings`. Pinning `vfio_user`
  to 0.1.6, the version Cloud Hypervisor locks, fixes the build. Every Cloud Hypervisor upgrade must compare its
  `Cargo.lock` with ours.
- The embedding surface is small and is the same one Cloud Hypervisor's own `main` uses:
  `vmm::start_vmm_thread`, an `mpsc` request channel with an `EventFd`, `vmm::api::{VmCreate, VmBoot}`, and
  `VmConfig::parse(VmParams)`. `VmParams` takes the same strings as the Cloud Hypervisor command line (`boot=1`,
  `size=256M`, `file=…`), which is the most stable way to build a configuration because it is a documented user
  interface. It is a struct literal, so a release that adds a field breaks our build loudly rather than silently.
- No `unsafe` is needed on Amafu's side. Cloud Hypervisor's main installs a `SIGSYS` handler and ignores `SIGCHLD`
  with `unsafe` calls; the per-VM process needs neither, because Amafu will apply its own seccomp filter (milestone
  2) and the process has no children.
- Direct kernel boot accepts Ubuntu's signed `bzImage`, so the test guest boots the node's own kernel with a small
  busybox initramfs.
- Nested inside an LXD VM on the lab, `amafu vm create --wait` reports the VM running about 2 seconds after the call
  (the lab budget is 5 seconds), and the guest's serial console shows its init running.

## Decision

- `amafu-hypervisor` embeds `vmm` in the node binary's `vm-process` command. Each VM gets one such process, started
  from `/proc/self/exe`, which reads a plan file (CPU count, memory, kernel, initramfs, command line, serial log)
  from the VM's runtime directory, boots the VM and writes a `booted` marker.
- The node-side adapter implements `Hypervisor` by spawning, finding (by process identifier and command line, so a
  reused process identifier is never mistaken for a VM) and stopping these processes. Its runtime directory lives on
  `/run`, so it survives a role restart but not a reboot, as the VMs do.
- The real adapter passes the same `hypervisor_contract` as the simulator's fake; the lab runs it through the
  diagnostic channel (`cargo rig lab contract`).

## Consequences

- The fallback (the Cloud Hypervisor binary over its HTTP socket) is not needed for milestone 1.
- Milestone 2 adds what the plan requires before the VM starts: a dedicated user per VM, cgroup placement, file
  descriptors passed over the control socket, Landlock, then seccomp. The VMM is started today with
  `SeccompAction::Allow` and Landlock off, and the process runs as root; that is acceptable only while the node
  runs nothing but test guests.
- Stopping a VM sends `SIGTERM`, which Cloud Hypervisor's signal handler turns into an immediate VMM exit. A graceful
  guest shutdown through ACPI is milestone 2 work.
- `VmBoot` returning is what the node reports as `Running`. The test-only diagnostic channel also exposes the
  guest's console, which shows that the guest really booted.
