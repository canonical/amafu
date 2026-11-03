# 0007: The VM process sandbox

Status: accepted for milestone 2

## Context

A VM process runs guest-controlled code paths (virtio device emulation) on the node. The plan requires that a
compromised VM process can reach neither the node's state nor other VMs.

## Decision

Each VM process is confined five ways, all set up by the node before the VMM starts:

1. **Identity.** Each VM gets its own user, allocated from 200000 upwards and recorded in its directory, and the
   shared group 64000, which alone may open `/dev/kvm`. Its directory is mode 0700 and its writable disks 0600.
2. **cgroup.** `/sys/fs/cgroup/amafu-vms/vm-<id>` limits CPU to the VM's vCPUs, memory to the VM's memory plus 256
   MiB with swap off, and processes to 4096. The node places the process in the cgroup before the VM exists.
3. **Hand-over.** The process starts with nothing but a socket pair as its standard input. The node sends the plan
   as one JSON message and the tap file descriptors with `SCM_RIGHTS`, so the process never opens a tap or needs
   `CAP_NET_ADMIN`.
4. **Landlock.** Reads are limited to the boot files, the image cache, read-only disks, `/dev/urandom`,
   `/dev/null`, `/proc` and `/sys`; writes to the VM's directory, its writable disks and `/dev/kvm`.
5. **seccomp.** A deny filter kills the process on `ptrace`, `process_vm_*`, `mount`, `pivot_root`, `chroot`,
   module loading, `kexec`, `bpf`, `unshare`, `setns`, key management, `perf_event_open`, `open_by_handle_at`,
   `reboot`, `swapon` and any `socket` that is not `AF_UNIX`. A second filter answers `io_uring_*` with `ENOSYS`,
   because io_uring operations bypass seccomp; Cloud Hypervisor falls back to its other block back ends. Cloud
   Hypervisor's own per-thread filters then apply with `KillProcess`.

The same process can be started in probe mode, sandboxed identically, to attempt one forbidden thing.
`/diagnostics/sandbox` runs eight probes; the hostile-guest scenario requires all of them to be blocked, and
`crates/node/tests/sandbox.rs` runs the same probes as root on a developer machine.

## Consequences

- The node itself still runs every role in one root process (decision 0005).
- Landlock reads of `/proc` and `/sys` are allowed because Cloud Hypervisor needs them; other users' processes
  remain protected by their uid.
- A VM process that fails reports its last error in the VM's status, so a misconfiguration is visible rather than a
  silent restart loop.
