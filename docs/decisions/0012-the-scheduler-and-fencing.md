# 0012: The scheduler, singleton controllers and fencing tokens

Status: accepted for milestone 3 (replaces the placement stand-in of 0005)

## Decision

- **`amafu-scheduler` is a pure crate**: `place(scheduler, cluster, pending)` sorts by priority, filters each node,
  scores the survivors and reserves capacity before deciding the next workload. Filters: cordoned, capacity (CPU
  overcommitted four times, 512 MiB of memory kept for the node), CPU baseline (x86-64 levels from the node's CPU
  flags), storage reachability (local volumes pin a VM to their node; images a VM needs must be cached there) and
  anti-affinity. One score, least allocated, with zone spread and then the node name breaking ties. A rejection
  explains how many nodes each filter turned away. 10,000 workloads on 1,000 nodes place in about 0.5 s
  (`cargo bench -p amafu-scheduler`).
- **The scheduler controller writes its decision into the VM's status** (node and interface addresses), as the
  placement stand-in did, instead of a separate `NodeAssignment` kind. A node already receives the VMs placed on it,
  and one object per VM keeps the ownership change a single write.
- **Every core controller runs as a Quorium singleton**: scheduler, network, security group, volume, storage
  backend, floating IP, external network, certificate revocation and node health. `reconciler::run_singleton` runs a
  controller only while its lease is held and drops it the moment the lease is lost. This also makes allocations
  (segment identifiers, floating addresses, interface addresses) single-writer. Image, local volume, compute and
  node-network controllers run on every node.
- **Scheduler writes carry the lease's fencing token.** `Store::compare_and_swap_fenced` stores, per object and per
  fence name, the highest token that wrote it, and refuses an older one. In Quorium this lives in the object entity
  itself (version 2 of `amafu.object`, migrated from version 1), so the check and the write are one atomic entity
  method. The simulator records every fenced write and the invariant "no write is accepted with a stale fencing
  token" checks the record after every step.

## Consequences

- Fencing is per object: a deposed scheduler cannot overwrite a placement a newer one made. It could still place a
  VM that no newer scheduler has touched yet; that placement is valid, and Quorium stops the old holder's job before
  its lease can be taken over, so the window is a lease renewal at most.
- Nodes outside the core cannot hold leases or fence writes; the node API refuses fenced writes.
