# 0006: Containers boot an erofs root file system in their own VM

Status: accepted for milestone 2

## Context

The plan runs each container in its own small VM and shares the image into the guest with virtio-fs. virtio-fs needs
a `virtiofsd` daemon per VM on the node: one more privileged process to sandbox, and one whose file-system view of
the image cache is the thing a hostile guest attacks.

## Decision

- The node converts a pulled OCI image into one read-only erofs file (`mkfs.erofs` over the unpacked layers, with
  whiteouts applied) and attaches it to the container VM as a read-only virtio block device.
- The writable layer is a tmpfs inside the guest, joined to the erofs root with overlayfs. A container's writes
  vanish when its VM stops; persistent data belongs on a volume.
- The guest runs a kernel built for it (milestone 4): Linux 6.18 long-term, configured from `allnoconfig` with only
  what a container needs (`image/container-kernel.config`: virtio, vsock, erofs, overlay, tmpfs, packet sockets),
  everything built in and booted uncompressed through its PVH entry. It is 17.6 MB instead of the node kernel's 71
  MB, and the build refuses a configuration that loses a requested option. The initramfs's init is
  `amafu-guest-agent`, which loads no modules. The agent mounts the root, takes a DHCP lease, writes
  `resolv.conf`, chroots into the container and waits on vsock port 1024 for the node to start the workload.
- The node talks to the agent over Cloud Hypervisor's hybrid vsock: a Unix socket in the VM's directory, `CONNECT
  1024`, then versioned JSON lines (`amafu_guest_agent::protocol`, fuzzed by `crates/guest-agent/fuzz`).
- The workload's exit code ends the VM: zero reports the container stopped, anything else reports it failed with
  the code, and neither is restarted.

## Consequences

- There is no shared file system between node and guest, and no `virtiofsd`. The guest reaches the image only as
  blocks it cannot write.
- An image is converted once per node and shared read-only by every container that uses it.
- The guest kernel skips the i8042 probe (`initcall_blacklist=i8042_init`, 1.2 s under nested virtualization) and
  boots from `vmlinux` to avoid decompressing itself. In the lab a container runs about 3 s after `amafu container
  run` returns, the budget the scenarios check.
