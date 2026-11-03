# 0011: State across nodes: three voters, and a subscribed share for every other node

Status: accepted for milestone 3 (refines 0002)

## Decision

- **Core members run three Quorium voters over mutual TLS.** A node with the `core` role and more than one entry in
  `state.voters` starts Quorium with static discovery and three replicas. Every voter presents its node certificate,
  which names `amafu-state` as a DNS name, the name Quorium verifies. A one-node cluster still starts in Quorium's
  single-node mode.
- **Every core member serves the API.** The rig and the command line can use any of them; a write through a member
  that has lost the majority fails as unavailable.
- **Other nodes subscribe to their share of the state through a core member**, over the same HTTPS listener as the
  API, under `/v1/node/`. The share is decided by `domain::slice::NodeSlice`, a pure function of the stored objects:
  the node's own `Node`, the VMs placed on it, their volumes and volumes it holds, network endpoints of the projects
  it hosts, floating IPs bound to it, and the small cluster-wide kinds (images, storage backends, networks, security
  groups, external networks). Certificate revocations are never sent.
- **The subscription is a stream of changes**: a snapshot first, then upserts and removals, with a heartbeat every
  five seconds. The core member recomputes the node's share whenever any kind changes and sends the difference, so
  an object that enters or leaves the share (a VM placed on the node brings its volumes) is handled like any other
  change. The node keeps the share in a local state cache on its data partition (`state-cache.json`).
- **`SubscribedStore` implements `Store` on a node outside the core.** Linearizable reads and lists go to a core member
  and are filtered to the share; stale reads come from the cache; writes are forwarded compare-and-swaps. Every
  existing controller therefore runs unchanged on such a node. The node-network controller falls back to the cache
  when the core is unreachable, so DHCP, DNS and the datapath keep working, also after a restart while cut off.
- **A node may write only its own share.** `NodeSlice::may_write` allows status changes of its own `Node` and of the
  VMs placed on it, the provisioning of local volumes it holds, its own entry in an image's cache list, its own
  network endpoints, and registering itself with no more roles than its certificate names. Every replacement must
  also be a valid object of its kind. Writes and every refusal are audited under `node:<name>`.
- **Nodes that stop sending heartbeats are marked not ready** by a `node-health` singleton after 30 seconds, and the
  scheduler places nothing on them.

## Consequences

- Quorium has no client-side watch streams and no non-voting members, so the subscription is Amafu's own, served by
  core members as the plan's simplification 10 allows. A relay role is added only if scale demands it.
- The node lease with an epoch that serves as the node's fencing token is milestone 4, together with fencing; the
  heartbeat and `node-health` are what milestone 3 needs to stop placing on lost nodes.
- The kind catalog is still one entity per kind (0002). Four nodes and the scheduler benchmark do not need sharding
  yet; the 1,000-node scenario in milestone 6 decides.
- A core member computes a subscriber's share from a full listing on every change. That is simple and correct for
  the lab; the 1,000-node scenario will show whether it needs an index.
