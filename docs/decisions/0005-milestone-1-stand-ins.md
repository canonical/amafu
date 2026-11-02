# 0005: Stand-ins used in milestone 1, and what replaces them

Status: accepted for milestone 1

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
| Every role runs in one `amafu-node` process, and a failed role is restarted with backoff inside that process | One node, one binary; crash-only behaviour is exercised by the simulator, which crashes roles at random points | One process per role with its own seccomp filter (milestone 2), supervised by PID 1 |
| A VM that cannot start because its specification is invalid (for example a missing image) is reported `failed` with the reason, and is retried only when its specification changes | The architecture says an invalid specification writes a condition to the status and waits for the specification to change | Conditions on the status, alongside the phase |

None of these stand-ins weakens a guarantee the plan says must not be simplified: generations on every object,
audit of every request including denials, at most one running instance of each VM, and signed images. Each of
those is checked by an invariant or a scenario at both levels.
