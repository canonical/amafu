# 0018: Node leases, storage fencing and the watchdog

Status: accepted for milestone 4 (replaces the heartbeat stand-in of 0005)

## Decision

- **Every node holds a `NodeLease`** (one per node, admin-only): an epoch, when it was last renewed by a core
  member's clock, and the terms the node runs with (lease and watchdog interval, 8 s each by default, 2 to 60 s
  allowed). Acquiring always starts a new epoch. A node outside the core acquires and renews through the core
  (`/v1/node/lease/acquire` and `/renew`); a core member does it through its own store.
- **The epoch fences the node's writes.** A node's own roles write through `LeaseFencedStore`, which refuses to write
  without a valid lease and otherwise writes with the fence `node:<name>` at the current epoch. The core's node API
  checks the presented epoch against the lease before every write, and the store's per-object fence refuses older
  epochs. A lost node can neither renew nor write.
- **The watchdog backs the lease.** The node feeds the kernel software watchdog (`softdog`, its margin set to the
  configured interval when it is armed on the first valid lease) only while its lease is valid by its own clock. A
  node that cannot renew stops feeding and is reset within one interval. Compute starts no VM and moves none
  without a valid lease.
- **Recovery is an operation the `node-health` singleton starts** when a lease has expired: mark the node lost (a new
  epoch and the `lost` phase), revoke it on every storage backend (NVMe/TCP removes the host from every subsystem,
  which drops its connections; Ceph blocklists the node's underlay address through the `restful` manager module),
  wait out the watchdog deadline (last renewal + lease + watchdog interval), then release its VMs to `pending`,
  withdraw its network endpoints, readmit it on storage and mark the lease released. The scheduler then places the
  VMs by priority. If revocation fails the operation stays on that step: Amafu prefers downtime to two writers.
- **A lost node may hold a lease again only once its VMs are released.** By then its watchdog has reset it, so it
  starts with nothing running.

## Consequences

- With the defaults a powered-off node's VMs run elsewhere about 20 s after it stopped; the budget is 30 s.
- A node cut off for longer than its lease resets itself, even if it runs VMs. A shorter cut keeps everything
  running, which the "node outside the core" scenario now checks with a 5 s cut.
- NFS and local disks have no fencing of their own; for them the watchdog deadline is the only guarantee.
- Ceph blocklisting needs the backend's command endpoint (`command_endpoint`, `command_user`, `command_key` and an
  optional CA) in addition to the dashboard.
