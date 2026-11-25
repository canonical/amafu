# 0035: The dashboard runs on the nodes, and the dashboard and the API share one cluster address

Status: proposed (replaces the deployment part of 0026)

## Context

The dashboard runs on separate Ubuntu machines that MAAS deploys (`amafu dashboard enable`). They share a keepalived address, and every release upgrades them on its own. That has three costs:

- An install needs MAAS, apt and keepalived. The planned ISO install for bare metal and small edge sites has none of them.
- Every cluster upgrade has a second, separate step.
- The separation buys less than it seems to. The dashboard holds no state and forwards every call to the API, so when the core members lose their store, the separate machines stay up but show only errors.

The CLI, guests (0031) and Juju reach the API through a list of node addresses, because the cluster has no address of its own.

## Decision

- **A `dashboard` node role.** A node with it serves the dashboard. Core members get it by default, and an operator can add it to or remove it from any node. It confers no authority: the dashboard only ever forwards the signed-in user's token (0026).
- **The image carries the dashboard.** It is a subcommand of `amafu-node`, so every node upgrade upgrades it and the separate dashboard machines go away.
- **It runs as its own sandboxed process.** It is not a task inside the control process, because it is the most exposed web surface on the node. It has its own user and cgroup, Landlock and seccomp like a VM process (0007), and is handed everything it needs over a socket pair. It never reads the store, the node's identity or the token file.
- **One cluster address serves both the dashboard (443) and the API (7443).** Exactly one dashboard node holds it at a time, and the other dashboard nodes elect a new holder without the store, so the address keeps answering while the store has no quorum.
- **A planned restart hands the address on first.** Before a node reboots for an upgrade, it gives the address to another dashboard node, then hands off store leadership as in 0034.
- **What the address needs lives in the store.** The address itself, its certificate and private key, and the dashboard's session key are in the store. Each dashboard node also keeps a copy on its data partition, so it can still serve after a restart during a store outage.
- **Deferred:** a degraded view built from node-local diagnostics, and a break-glass login that works without quorum.

## Architecture

### Overview

```
                    cluster address 10.20.0.50 (held by exactly one dashboard node)
                          │ 443 dashboard        │ 7443 API
┌─────────────────────────┼──────────────────────┼──────────────────────┐
│ dashboard node          ▼                      ▼                      │
│  ┌──────────────────────────┐   loopback   ┌───────────────────────┐  │
│  │ dashboard process        │ ───────────► │ control process       │  │
│  │ (sandboxed, no authority)│  user token  │  API, store, roles    │  │
│  └──────────────────────────┘              │                       │  │
│              ▲ socket pair: TLS identities,│  DashboardRole        │  │
│              └──── session key, listeners ─│  ClusterAddressRole ──┼──┼── advertisements (UDP)
│                                            └───────────────────────┘  │    to the other dashboard nodes
└───────────────────────────────────────────────────────────────────────┘
```

### Domain (`amafu-domain`)

| Type | Kind | Who may change it | Holds |
|---|---|---|---|
| `NodeRole::Dashboard` | `Node` role | nodes register it; operators add or remove it | whether the node serves the dashboard |
| `ClusterAddress` | cluster singleton `cluster` | operators | `address` (IPv4 with prefix length) and an optional `interface` (default: the underlay) |
| `ClusterAddressCertificate` | cluster singleton, `ClusterPolicy::NodeSecret` | admins write, only nodes read | certificate chain and private key for the address |
| `DashboardSessionKey` | cluster singleton, `ClusterPolicy::NodeSecret` | created by the cluster itself | the key that seals dashboard sessions |
| `NodeStatus::holds_cluster_address` | `Node` status | the node | whether this node holds the address, for the CLI and the dashboard |

- `ClusterPolicy::NodeSecret` is new, and is the only new access rule:
  - admins may create, replace and delete such an object, but never read it back;
  - nodes may read it;
  - its specification is never written to the audit log.
- `Core` and `Compute` stay in the node's certificate and configuration, because they confer authority. `Dashboard` lives only in `NodeSpecification::roles`:
  - the node controller adds it when a core member registers;
  - `amafu node update <node> --add-role dashboard` or `--remove-role dashboard` changes it.
- **Version gating:** a node on an older version cannot read the new role or the new kinds. The API therefore refuses them until the cluster's finalized feature level includes them, and the node controller adds the role only after that.

### The address election (new crate `amafu-cluster-address`)

The election is a small state machine, kept apart from all I/O so it can be read and tested on its own.

```rust
pub struct Candidate { pub node: NodeName, pub is_eligible: bool }

pub struct Advertisement { pub node: NodeName, pub is_eligible: bool, pub holds: bool, pub sent_at: Instant }

pub enum Claim { Hold, Release }

pub struct Election { me: Candidate, heard: BTreeMap<NodeName, Advertisement>, holds: bool }

impl Election {
    pub fn hear(&mut self, advertisement: Advertisement);
    pub fn decide(&mut self, now: Instant) -> Claim;
    pub fn advertisement(&self, now: Instant) -> Advertisement;
}
```

`decide` applies four rules, in this order:

1. **Ineligible:** a node that is not eligible releases the address.
2. **Incumbent:** a holder keeps the address while it is eligible. No node takes it away from a healthy holder, so the address doesn't flap.
3. **Vacancy:** when no peer heard within the last `SILENCE` (3 s) holds the address, the eligible node with the lowest name claims it.
4. **Two holders:** if two nodes hold it, the higher name releases. This only happens after a network partition heals.

A node is **eligible** when:
- its `Node` specification has the dashboard role (read from its local copy);
- its dashboard process answers on loopback;
- it is not leaving for a planned restart.

**Store health doesn't count.** The address must stay up during a store outage.

The I/O sits behind three small adapters:
- **`Advertiser`:** sends and receives one advertisement per second over UDP on the underlay, to every other dashboard node.
  - Peers come from the local copy of the dashboard nodes, refreshed from the store when it is available.
  - Messages are authenticated with HMAC-SHA256 under a key derived from the session key.
- **`AddressHolder`** (in `amafu-network`, reusing `floating.rs` and `announcement.rs`):
  - `hold` adds the address to the interface and sends gratuitous ARP;
  - `release` removes it.
- **`ClusterAddressRole`** (in `amafu-node`): runs the loop hear → decide → hold or release → advertise, and reports `holds_cluster_address` on the node's status.

Why not keepalived: the node image has no package manager, keepalived would be a second daemon with its own configuration, and VRRP needs multicast. The protocol Amafu needs is about a hundred lines and can be tested with a virtual clock.

### Serving

- **`DashboardRole`** (in `amafu-node`, control process): supervises the dashboard process with the existing restart backoff. It:
  - opens the listening socket on `0.0.0.0:443`, so the process needs no privilege;
  - reads the session key and both TLS identities: the node's own, and the cluster address's;
  - starts `amafu-node dashboard` with nothing but a socket pair as its standard input;
  - sends it one JSON message with that configuration, plus the listener over `SCM_RIGHTS`, the same hand-over as VM processes;
  - restarts it when the certificate or the session key changes.
- **The dashboard process** reuses `amafu-dashboard` unchanged except for its entry point.
  - Its upstreams are the local API first, then the other core members, through the existing failover in `DashboardState::open`.
  - It does not trust the cluster CA through any file; the CA arrives in the hand-over message.
- **Certificates by local address.** A new `CertificateByLocalAddress` (in `amafu-api`) chooses the TLS identity for each accepted connection from the address it arrived on:
  - the cluster address gets its own certificate;
  - every other address gets the node's certificate.

  The API and the dashboard both use it. Clients that connect by IP send no server name, so the choice can't rely on SNI.
- **The API needs no change to listen:** it already binds `0.0.0.0:7443`, so it answers on the cluster address as soon as the address is on the interface.

### Planned restarts

`BootSlotsController::leave_and_reboot` becomes one readable sequence:

```rust
async fn leave_and_reboot(&self) -> Result<(), ActionError> {
    self.cluster_address.step_aside().await;
    self.consensus.hand_off_leadership().await?;
    self.surrender_lease().await;
    self.consensus.close().await;
    self.boot_slots.reboot().await.map_err(boot_slot_failure)
}
```

`step_aside` makes the node ineligible, releases the address, advertises the release at once so a peer claims it without waiting for `SILENCE`, and waits up to 3 s until another node reports holding it. A node that doesn't serve the dashboard gets a no-op, like `NotAVoter`.

### Install and the command line

- **The install description** gets `cluster_address: 10.20.0.50/24`. The installer:
  - issues the address's certificate from the cluster authority it holds;
  - writes `ClusterAddress` and `ClusterAddressCertificate` once the store is up;
  - writes the cluster address as the first endpoint in the credentials it hands back.

  The ISO installer will ask for the same value.
- **`amafu cluster-address set <address/prefix>`:**
  - changes the address;
  - with `--state-directory`, issues the new certificate from the installer's authority;
  - with `--certificate` and `--private-key`, uses an operator's certificate instead.
- **A core singleton creates `DashboardSessionKey`** the first time a dashboard node needs it, so no human ever handles it.
- **`amafu dashboard enable`, `create-session-key`, `management.rs` and the dashboard step of `lab-release.sh` are removed.** `amafu dashboard serve` stays, for running the dashboard outside the cluster.
- **`amafu node list`** shows the dashboard role in ROLES and marks the address holder.

## Implementation plan

Each step leaves the cluster working and is its own set of small commits.

1. **Domain:**
   - the `Dashboard` role and `ClusterPolicy::NodeSecret`, with its authorization and audit tests;
   - the `ClusterAddress`, `ClusterAddressCertificate` and `DashboardSessionKey` kinds;
   - `NodeStatus::holds_cluster_address`;
   - feature-level gating.
2. **Role management:** the node controller adds the role to core members on registration; `amafu node update --add-role/--remove-role`; the ROLES column in the CLI and the dashboard.
3. **The election:** `amafu-cluster-address`, with table-driven tests of the four rules and a virtual-clock test of failover, flapping and a healed partition.
4. **The address on the interface:**
   - `AddressHolder` in `amafu-network` and the advertiser;
   - `ClusterAddressRole` and the status report;
   - a scenario in the simulated world with three dashboard nodes: the holder dies, a partition, a partition healing.
5. **Certificates:** `CertificateByLocalAddress` in `amafu-api`, used by the API listener, with a test that a connection to each address sees the right certificate.
6. **The dashboard on the node:**
   - the `amafu-node dashboard` subcommand and its sandbox;
   - `DashboardRole`, the local copy of what the role and the election need;
   - the sandbox probes added to `/diagnostics/sandbox`.
7. **Planned restarts:** `step_aside` in `leave_and_reboot`, with a boot-slots test of the order.
8. **Install and the command line:** the installer field, `amafu cluster-address set`, the endpoint in the credentials, and the guides.
9. **Lab migration:**
   - roll the release out;
   - finalize it so the new kinds are allowed;
   - set 10.20.0.50 as the cluster address;
   - check the dashboard and the API on it;
   - stop and delete the dashboard machines;
   - remove the MAAS dashboard deployment and the dashboard step of `lab-release.sh`.
10. **Lab validation:**
    - Probe `https://10.20.0.50/` and `https://10.20.0.50:7443/v1/health` every 200 ms throughout a full rollout. The address should never be unreachable for more than one probe.
    - Force-stop the holder: a new holder should answer within `SILENCE` plus one advertisement, about 4 s.
    - Cut the store's quorum on purpose: the address must keep answering. The dashboard may only report that the cluster is unavailable.

## Consequences

- Every node upgrade upgrades the dashboard. The release script and the operator have one step fewer.
- The dashboard shares hardware with the store and with guests. Its cgroup limits it to 1 CPU and 512 MiB by default, reserved in the node's capacity like the node agent's own share.
- The cluster address needs the dashboard nodes on one L2 segment, because ownership is announced with gratuitous ARP. Routed fabrics need BGP announcement, which 0013 already plans for floating IPs.
- A single-node cluster holds the address on its only node, and the address goes away while that node reboots.
- While the store has no quorum, the dashboard stays reachable but can only say that the cluster is unavailable, until the degraded view is built.
