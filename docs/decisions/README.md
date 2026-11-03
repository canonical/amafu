# Decision records

Each record states a decision that is hard to reverse, why it was taken, and what it commits us to.

| Record | Decision |
|---|---|
| [0001](0001-embed-cloud-hypervisor-as-a-library.md) | Embed Cloud Hypervisor's `vmm` crate in a per-VM process (milestone 1 spike) |
| [0002](0002-state-on-a-single-voter-quorium.md) | Store state on a single-voter Quorium node behind the `Store` trait (milestone 1 spike) |
| [0003](0003-boot-chain-and-root-file-system-in-the-lab.md) | Boot the lab through the distribution's signed chain, with the whole system in the initramfs |
| [0004](0004-roles-and-authorization.md) | What the four fixed roles may do |
| [0005](0005-stand-ins.md) | Stand-ins used in milestones 1 and 2, and what replaces them |
| [0006](0006-containers-boot-an-erofs-root-in-their-own-vm.md) | Containers boot an erofs root file system in their own VM, instead of virtio-fs |
| [0007](0007-the-vm-process-sandbox.md) | The VM process sandbox: identity, cgroup, descriptor hand-over, Landlock and seccomp |
| [0008](0008-private-networks.md) | Private networks: addresses at placement, bridges, nftables and local services |
| [0009](0009-images-and-volumes.md) | Images fetched by digest, volumes as qcow2 overlays, storage backends and disk boot |
| [0010](0010-the-boot-partition-holds-what-the-node-reads-on-demand.md) | The boot partition holds what the node reads on demand (refines 0003) |
