# Decision records

Each record states a decision that is hard to reverse, why it was taken, and what it commits us to.

| Record | Decision |
|---|---|
| [0001](0001-embed-cloud-hypervisor-as-a-library.md) | Embed Cloud Hypervisor's `vmm` crate in a per-VM process (milestone 1 spike) |
| [0002](0002-state-on-a-single-voter-quorium.md) | Store state on a single-voter Quorium node behind the `Store` trait (milestone 1 spike) |
| [0003](0003-boot-chain-and-root-file-system-in-the-lab.md) | Boot the lab through the distribution's signed chain, with the whole system in the initramfs |
| [0004](0004-roles-and-authorization.md) | What the four fixed roles may do |
| [0005](0005-stand-ins.md) | Stand-ins used in milestones 1 to 5, and what replaces them |
| [0006](0006-containers-boot-an-erofs-root-in-their-own-vm.md) | Containers boot an erofs root file system in their own VM, instead of virtio-fs |
| [0007](0007-the-vm-process-sandbox.md) | The VM process sandbox: identity, cgroup, descriptor hand-over, Landlock and seccomp |
| [0008](0008-private-networks.md) | Private networks: addresses at placement, bridges, nftables and local services |
| [0009](0009-images-and-volumes.md) | Images fetched by digest, volumes as qcow2 overlays, storage backends and disk boot |
| [0010](0010-the-boot-partition-holds-what-the-node-reads-on-demand.md) | The boot partition holds what the node reads on demand (refines 0003) |
| [0011](0011-state-across-nodes.md) | Three voters over mutual TLS, and a subscribed share of the state for every other node |
| [0012](0012-the-scheduler-and-fencing.md) | A pure scheduler, singleton controllers and per-object fencing tokens |
| [0013](0013-overlay-networks-and-floating-ips.md) | VXLAN overlays with static forwarding, and floating IPs announced by gratuitous ARP |
| [0014](0014-volumes-over-the-network.md) | NVMe/TCP and Ceph RBD volumes, local volumes on their node, images on every node |
| [0015](0015-node-identity.md) | Node certificates, revocations, and refusing rogue nodes |
| [0016](0016-the-virtual-lab-topology.md) | Four nodes, a lab switch and storage targets on the underlay |
| [0017](0017-the-operations-runner.md) | Operations run as recorded, idempotent steps |
| [0018](0018-node-leases-fencing-and-the-watchdog.md) | Node leases fence a node's writes; storage fencing and the watchdog come before any restart |
| [0019](0019-live-migration-and-evacuation.md) | Live migration and evacuation as operations, relayed between nodes over mutual TLS |
| [0020](0020-init-and-control.md) | A small init supervises one control process with fewer capabilities |
| [0021](0021-boot-slots-and-upgrades.md) | Two boot slots chosen through grubenv, and upgrades and rollbacks as operations |
| [0022](0022-install-from-maas.md) | Install from MAAS through an in-memory Ubuntu, with a local journal |
| [0023](0023-storage-plugins-as-webassembly-components.md) | Storage plugins as WebAssembly components with a host that holds every capability |
| [0024](0024-reads-at-scale.md) | Reads at scale from a replica on each core member, and no subscription relays |
| [0025](0025-audit-export.md) | Audit export as a singleton that keeps its place in the sink's status |
| [0026](0026-the-dashboard.md) | A stateless dashboard that forwards each user's token, in React and Vanilla |
