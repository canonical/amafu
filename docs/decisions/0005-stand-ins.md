# 0005: Stand-ins, and what replaces them

Status: accepted for milestones 1 to 5

Milestone 1 builds every layer thinly. Where the plan's full design belongs to a later milestone, milestone 1 uses
a simpler stand-in behind the same interface, so that replacing it does not ripple through the other crates.

| Stand-in | Why | Replaced by |
|---|---|---|
| A placement controller on the core node assigns each pending VM to the first ready, uncordoned compute node by name, writing the node into the VM's status | There is one node, and the scheduler is milestone 3 | The scheduler, a pure placement function run as a Quorium singleton that writes assignments with its fencing token (milestone 3) |
| Bearer tokens, stored as SHA-256 digests in the node configuration, with an optional expiry | Authentication must exist and be tested from day one; the cluster CA that would issue client certificates does not exist yet | Mutual TLS client certificates and OIDC tokens, behind the same `Authenticator` trait |
| The node's API uses a TLS certificate and key from its configuration; the rig issues them from a per-lab CA | There is no cluster CA yet | Certificates issued by the cluster's intermediate CA at install (milestone 5) |
| The underlay gets a static address from the node configuration, set over netlink | The image has no DHCP client and the kernel has no in-kernel autoconfiguration | The node configuration's `network` section with bonds and DHCP from MAAS (milestones 3 and 5) |
| The test-only diagnostic channel is plain HTTP on a TCP port reached through an LXD proxy device, not a virtio serial port | LXD does not relay a guest's extra serial ports to API clients | Keep the transport behind the `Cluster` trait. The channel exists only in test builds (`--features diagnostics`); a release build ignores its configuration |
| The lab console is read by attaching to LXD's serial console | LXD 6.10 has no console log for VMs | A console log in the rig's run reports |
| Every role runs in one `amafu-node` process, and a failed role is restarted with backoff inside that process | One node, one binary; crash-only behaviour is exercised by the simulator, which crashes roles at random points. Milestone 2 sandboxed the VM processes, which run guest-facing code (decision 0007), and kept the roles together | One process per role with its own seccomp filter, supervised by PID 1 (milestone 3) |
| A VM that cannot start because its specification is invalid (for example a missing image) is reported `failed` with the reason, and is retried only when its specification changes | The architecture says an invalid specification writes a condition to the status and waits for the specification to change | Conditions on the status, alongside the phase |

Milestone 2 adds:

| Stand-in | Why | Replaced by |
|---|---|---|
| The image controller runs on the core node, caches images there and reports `cached_on` as that node | The lab has one node | An image controller on every compute node, each adding itself to `cached_on`, with deletion waiting for every node (milestone 3) |
| Networks, local services and `local-disk` volumes span one node | Overlay networking and volume migration need several nodes | VXLAN overlays between nodes and volume moves (milestone 3) |
| The lab's NFS server is an Ubuntu VM in the lab project, exporting with `no_root_squash` | It is the smallest real NFS server | Any NFS server the operator provides |

Milestone 3 replaces the placement controller (0012), the single voter (0011), the image controller on the core
node and single-node networks and local volumes (0013, 0014). It adds:

| Stand-in | Why | Replaced by |
|---|---|---|
| Every role still runs in one `amafu-node` process | Agreed to defer: milestone 3 is large, and the split belongs with the failure work | One process per role, supervised by PID 1 (milestone 4) |
| Nodes report heartbeats and a `node-health` singleton marks silent nodes not ready | Placement only needs to avoid lost nodes until restarts exist | Node leases renewed through the subscription, whose epoch is the node's fencing token (milestone 4) |
| The rig issues node certificates and writes them into the node configuration | There is no installer or cluster authority yet | Certificates issued at install, bound to the TPM and rotated by the cluster (milestone 5) |
| Storage backend credentials are stored in the backend's specification | There is no secret store yet | Secrets stored encrypted and handed out by reference |
| Block volumes start empty | Cloning an image into a block device needs a qcow2 reader on the node | Image population for NVMe/TCP and RBD volumes |
| One MicroCeph VM with one replica, no MAAS lab, no FRR router | The lab server has 24 GiB of memory | Three MicroCeph VMs, the MAAS lab with bonded underlays and BGP for floating IPs, on a larger lab |
| The rig drives the lab switch and storage targets through LXD exec | Fewer moving parts than an agent | Keep, unless faults need to be faster than an exec call |
| Containers boot Ubuntu's 71 MB guest `vmlinux` (replaced in milestone 4 by a 17.6 MB kernel built for containers, 0006). In the nested lab a node's first container after boot runs in about 4 seconds, over the 3-second budget, and later ones in 1.6 to 2 seconds | Loading the kernel takes 3.8 seconds cold, about 0.6 seconds after the boot-time read-ahead (0010) and 2 milliseconds warm; the nested guest boot adds the rest. Accepted as a known gap of milestone 3 | A small purpose-built container kernel (milestone 4) |

Milestone 4 replaces the single process (0020) and the heartbeats (0018). It adds:

| Stand-in | Why | Replaced by |
|---|---|---|
| Nodes are powered off by forced LXD stops, and the soak runs 50 of them | There is no MAAS lab, and 500 stops take most of a day | 500 forced stops weekly and 50 power-offs through MAAS in the MAAS lab |
| Every simulator run covers 1,000 seeds | 10,000 seeds take hours on one machine | 10,000 seeds nightly |
| The kernel's software watchdog (`softdog`) | LXD exposes no emulated watchdog device | A hardware watchdog on real nodes |
| Migrations use one connection and send zero pages | Cloud Hypervisor's receiver supports nothing else through a unix socket | Parallel TLS streams and zero-page skipping, contributed upstream |

Milestone 5 replaces the rig issuing node certificates (the installer issues them, 0022) and adds:

| Stand-in | Why | Replaced by |
|---|---|---|
| The MAAS lab installs three machines with one core member, and the virtual lab is still built by the rig, with the same slots and signed images | The lab server has about 24 GiB, and the virtual lab's storage targets need most of it | A ten-node MAAS lab installing in under 30 minutes |
| MAAS commissions machines the rig creates and enlists with the LXD power driver, instead of composing them | The machines' NICs, MACs and boot order must be known to write the install description | Composed machines once the MAAS lab is larger |
| The twenty-node, 200-VM upgrade and rollback runs in the simulator on six seeds with role crashes; the lab upgrades and rolls back its four nodes with eight VMs | One seed of the large world takes about an hour with role crashes; the lab has four nodes | The twenty-node lab with 200 VMs in CI |
| The previous release is the same build with another release file, and the broken image has an empty initramfs | There is one build per lab run, and no CI keeps earlier releases | CI upgrading every build from the previous release |
| Node configurations, with their private keys, travel in MAAS user data | There is no join endpoint yet | A join token exchanged for a TPM-bound certificate at first boot |
| The two features of the plan are gates with nothing behind them | Neither feature exists yet | Parallel migration streams and dedicated subscription relays |

None of these stand-ins weakens a guarantee the plan says must not be simplified: generations on every object,
audit of every request including denials, at most one running instance of each VM, and signed images. Each of
those is checked by an invariant or a scenario at both levels.
