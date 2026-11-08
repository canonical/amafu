# 0024: Reads at scale from a replica on each core member, and no subscription relays

Status: accepted for milestone 6

## Decision

- **Every core member keeps a read replica** of all kinds except node leases: an in-memory copy fed by the
  store's watch, reloaded in place with differences after a resynchronisation, with a broadcast of changes per
  kind. The cluster API and the node API share it.
- **Reads that can be stale are served from the replica:** `?consistency=local` on the API, the node API's
  subscriptions and slices, and the revocation check that authenticates every node request. Writes and default
  reads stay linearizable through Quorium.
- **Subscriptions are incremental.** A node's full share is computed when it subscribes and when a VM arrives on it or
  leaves it; every other change is sent as a difference. A full view is sent again every twelve refreshes, spread over
  1,000 phases by a stable hash of the node's name, so a thousand nodes do not refresh together.
- **Nodes and resynchronisations are spread by that hash too:** each node prefers one core member, and each
  controller's periodic resynchronisation is offset per node.
- **The API runs on its own four-thread runtime**, so controllers on the same member cannot delay reads.
- **The audit log is written in batches** of up to 4,096 entries with one Quorium append, and audit records for
  reads are written without waiting.
- **Subscriptions do not need dedicated relays.** With the replica, three core members serve 1,000 nodes well within
  budget, so the `DedicatedSubscriptionRelays` feature level stays empty and the relay role is not written.

## Measurements

`cargo rig scale` runs three real core members on one machine against 1,000 simulated nodes that use the real
subscription client and node API.

| Run | Join | Placement of 2,000 VMs | Reads at 2,000/s | Core memory |
|---|---|---|---|---|
| Budget | 120 s | 120 s | p99 10 ms | 2,048 MiB |
| 1,000 nodes, state on tmpfs | 11.5 s | 26.8 s | p99 2.2 ms, median 0.28 ms, no failures | peak 586 MiB |
| 200 nodes (400 VMs), state on the host's network block device | 11.4 s | 89.4 s | p99 1.2 ms | peak 171 MiB |
| 1,000 nodes, state on the host's network block device | 17.6 s | did not finish in 260 s | not reached | — |

Quorium's writes on this host's network block device take 50 ms at the median and 265 ms at the 90th percentile,
against 0.2 ms on tmpfs; placement is a long series of fenced writes, so it is bound by the disk. tmpfs stands in for
the local NVMe the plan requires for core members (0005).

## Consequences

- A stale read can be behind a write that has already been acknowledged; clients that need their own writes ask
  for the default consistency.
- Each core member holds the whole cluster in memory, about 500 MiB at 1,000 nodes and 2,000 VMs.
