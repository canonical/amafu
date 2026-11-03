# 0016: The virtual lab topology

Status: accepted for milestone 3

## Decision

- **Four nodes**: three core members that also run workloads, and one compute-only node that lives on its subscribed
  share. Each node has a management interface on `lxdbr0` (default route, the rig's API and diagnostics proxies) and
  an underlay interface, selected by MAC address in the node configuration.
- **A lab switch VM owns every underlay link.** Each attached machine has its own LXD bridge (`amfl-*`, no address),
  and the switch has one port on each, bridged together as the underlay `10.216.0.0/24`. A partition is a pair of
  nftables bridge rules on the switch, applied atomically through LXD's exec API, so faults never touch the LXD host
  and never cut the rig's own access to nodes. The switch also probes floating IPs from outside the cluster.
- **Storage targets sit on the underlay**: an NFS server, an SPDK NVMe/TCP target and a single MicroCeph VM with three
  loop OSDs and one replica. The SPDK target is built from source once and published as a cached LXD image, which
  `cargo rig lab down` keeps.
- **The lab fits a server with 8 vCPUs, 24 GiB of memory and 61 GiB of disk**, as agreed for milestone 3. Images live
  in the default project (`features.images` off), so cached images survive the lab project.

## Consequences

- The plan's three MicroCeph VMs, the MAAS lab and the FRR router are deferred to a larger lab (0005).
- The rig talks to the switch and storage targets through LXD exec instead of an agent of its own; the fault logic
  lives in `test-rig/lxd-lab/src/switch.rs`.
