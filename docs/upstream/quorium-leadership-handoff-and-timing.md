# Feature request: deliberate leadership handoff, graceful shutdown, configurable timing and election visibility

Requested by: Amafu (control plane built on quorium)
quorium revision observed: `b0a97b08a9bffebf0c977692f5ac5dd64dd9fd7a` (`0.1.0-alpha.1`, openraft `0.10.0-alpha.37`)

## Summary

Amafu runs quorium with three voters, one per node, and reboots those nodes one at a time during rolling upgrades. Today a planned reboot of one voter can leave the other two without a leader for more than six seconds. Amafu then cannot write, its nodes cannot renew their fencing leases, and their watchdogs reset them: one planned reboot takes the whole cluster down.

quorium gives a caller no way to prevent this or even to see it coming. We ask for five things:

1. **Deliberate leadership handoff** that works while writes keep flowing.
2. **Graceful shutdown** that hands off leadership and closes the WAL before the process stops.
3. **Configurable timing**, with the derived values exposed, and an error instead of a silent clamp when inputs conflict.
4. **Election and leadership visibility**, so a caller can wait for a stable cluster before taking a voter down.
5. **A bounded, configurable write path** while the leader lease is expired, with a distinct error.

Defaults can stay as they are. Everything here is opt-in or additive.

## How Amafu uses quorium

- Three voters, static discovery, TLS, `Builder::heartbeat(500 ms)`; nothing else tuned.
- Each Amafu node holds a fencing lease of 8 s, renewed every 2 s by a quorium write. A node whose renewal has not succeeded for 8 s stops feeding a kernel watchdog (8 s) and is reset. This is deliberate: a node that cannot reach the store must not keep running workloads.
- So the store must elect a new leader and accept writes again well within about 6 s after losing one, or every surviving node fences.

## What we observed

Lab: three VMs (4 vCPU, 5 GiB) on one host. Rolling upgrade of 26.11.39 on 2026-11-24, serial consoles of every node recorded. All reboots themselves were clean (firmware about 14 s, new image serving about 25 s later).

Timeline of the second node's planned reboot (UTC):

| Time | Event |
| --- | --- |
| 16:58:37 | Upgrade writes node-2's inactive boot slot (about 6 s of heavy disk I/O on the shared host) |
| 16:58:45 | node-2 surrenders its Amafu lease and reboots. node-3 is the quorium leader |
| 16:58:50 | node-1: "renewing the node's lease took more than half the lease (6.07 s)", then "the store is unavailable: no leader can be reached" |
| 16:58:58 | node-3 (leader): `HeartbeatWorker ... failed to send a heartbeat: Err(Elapsed(()))` to both followers; node-3 logs `received RaftMsg::ExternalCommand, cmd: Heartbeat` every ~11 ms (the `LEASE_WAIT` loop in `service.rs`) |
| 16:59:08 – 16:59:14 | node-1 and node-3 are reset by their watchdogs; the whole cluster is down |
| 16:59:15 | node-2, booting, starts pre-vote elections; `while requesting pre-vote, error: ... request timed out` to both peers, repeatedly |
| 16:59:46 | All three serve again |

The same sequence repeated exactly when node-3 rebooted at 17:02:44. Across 17 minutes of node-2 and node-3 logs: 97 elections started, 277 heartbeat failures, 222 lease-expired write retries. The surviving pair never kept a leader through a planned reboot.

Effective timing with `heartbeat(500 ms)`, derived from the code:

| Setting | Value | Source |
| --- | --- | --- |
| Raft heartbeat | 500 ms | `Builder::heartbeat` |
| Ladder base | 1.5 s | `ladder_for`: `base.max(heartbeat * 3)` |
| Leader lease / election timeout | 1 s | `raft_config`: `heartbeat * 2` |
| Heartbeat and append RPC timeout | 1 s | openraft uses `heartbeat_interval`; quorium floors it at `MIN_RPC_TTL` |
| Campaign timeout / maximum | max(60 ms, 3×RTT) / max(300 ms, 6×RTT) | `Ladder::campaign_timeout`, `max_campaign` |
| Write wait while leader lease expired | 2 s, retrying every 10 ms | `LEASE_WAIT` in `service.rs` |
| Leadership transfer allowed | only after 2 s without writes | `TRANSFER_IDLE`, `transfer_ready` |

Two things stand out:

- **The leader is lost on a one-second stall.** Under the I/O and CPU burst of a neighbour booting, RPCs between the surviving VMs exceeded one second. Production hardware has more headroom, but a one-second disk or scheduling stall on one node, during the one moment the cluster runs with two voters, still costs the leader.
- **Re-election took far longer than any single timeout.** Pre-vote rounds kept timing out against peers that were themselves stalled. Nothing tells the caller how long recovery can take, so Amafu cannot size its fencing lease against it.

The leader that was about to reboot could not have avoided this either. Amafu writes continuously (lease renewals, status), so `transfer_ready` never holds and there is no public API to step down.

## Requested features

### 1. Deliberate leadership handoff

```rust
impl Quorium {
    pub async fn step_down(&self, options: StepDown) -> Result<HandoffReport, HandoffError>;
}

pub struct StepDown {
    pub deadline: Duration,
    pub stand_aside: Duration,
    pub prefer: Option<NodeId>,
}

pub struct HandoffReport {
    pub shards: Vec<ShardHandoff>,
}

pub struct ShardHandoff {
    pub shard: u64,
    pub new_leader: Option<NodeId>,
    pub took: Duration,
}
```

Behaviour:

- For every shard this node leads, pick a live, caught-up voter (`prefer` if given and eligible).
- Do not require `TRANSFER_IDLE`. If the target is behind, briefly hold new client writes on that shard, bounded by a few consensus round trips, until it has the leader's last log index; then `transfer_leader`. Held writes resume against the new leader, or fail with a retryable error, never silently dropped.
- Return once each shard has a confirmed new leader, or with `HandoffError::Deadline` listing the shards still led here.
- For `stand_aside`, this node does not stand for election (same mechanism as `handoff_suspend`), so it does not win leadership back while it shuts down.
- Calling it on a node that leads nothing is a no-op that succeeds.

Acceptance criteria (quorium-sim):

- With three voters and steady writes on every shard, `step_down` on the leader moves every shard's leadership within `deadline`, and no client write observes an unavailable error longer than one consensus round trip.
- After `step_down` plus `shutdown`, the other two voters keep a leader on every shard for the whole time the node is gone, with no election started by them.

### 2. Graceful shutdown

```rust
impl Quorium {
    pub async fn shutdown_gracefully(self, deadline: Duration) -> ShutdownReport;
}
```

Behaviour:

1. `step_down` with the remaining deadline.
2. Stop accepting new client writes, finish in-flight ones.
3. Flush and fsync the WAL and the state database, then close them.
4. Stop the RPC server and shard tasks.

`ShutdownReport` says which steps completed. The existing `shutdown`, which aborts tasks without handing off, stays for emergencies.

Why: Amafu currently syncs for at most 10 s and reboots. On one earlier occasion a node kept running after its data filesystem was already gone, logging `Input/output error (os error 5)` from openraft. A clean close before reboot removes that class of failure.

### 3. Configurable timing, validated, with the derived values exposed

```rust
impl Builder {
    pub fn ladder(self, ladder: LadderConfig) -> Self;
    pub fn rpc_timeout(self, minimum: Duration) -> Self;
    pub fn write_lease_wait(self, wait: Duration) -> Self;
}

impl Quorium {
    pub fn timings(&self) -> Timings;
}

pub struct Timings {
    pub heartbeat: Duration,
    pub leader_lease: Duration,
    pub election_timeout: Duration,
    pub rpc_timeout: Duration,
    pub ladder: LadderConfig,
    pub write_lease_wait: Duration,
    pub worst_case_failover: Duration,
}
```

Behaviour:

- `ladder`: `Builder.ladder` exists but is `pub(crate)` with no setter. Expose it.
- `rpc_timeout`: replaces the `MIN_RPC_TTL` constant (1 s) as a floor for consensus RPCs.
- `write_lease_wait`: replaces the `LEASE_WAIT` constant (2 s).
- `start` returns a `StartError::Timing` describing the conflict, instead of `raft_config` silently clamping the heartbeat to a third of the ladder base. Today `ladder_for` happens to widen the base first on the node path, but `quorium-sim` and any caller of `raft_config` get the clamp silently.
- `timings()` reports the effective values after derivation, including an estimate of worst-case failover: lease expiry plus the ladder's maximum campaign time for the configured cluster size. Amafu would check at startup that this fits inside its own fencing lease, and refuse to start otherwise.

### 4. Election and leadership visibility

```rust
impl Quorium {
    pub fn leadership_events(&self) -> impl Stream<Item = LeadershipEvent>;
}

pub enum LeadershipEvent {
    ElectionStarted { shard: u64, term: u64, pre_vote: bool },
    LeaderChanged { shard: u64, term: u64, leader: Option<NodeId> },
    LeaderLost { shard: u64, term: u64 },
    HeartbeatFailed { shard: u64, peer: NodeId },
}
```

Additions to `Status` (per shard): `leader_since`, `last_quorum_contact`.
Additions to `Stats`: `elections_started`, `leader_changes`, `heartbeat_failures`, `leaderless_time`.

Why: before rebooting a voter, Amafu should wait until every shard has had the same leader for a while, with no recent elections and every peer recently heard from. It cannot see that today; it only finds out when writes start failing. The same data would also make lab and field diagnosis possible without parsing openraft's INFO logs.

### 5. Bounded write path while the leader lease is expired

Today a write on a leader whose lease has expired retries every 10 ms, triggering a heartbeat each time, for up to 2 s. On a stalled node this adds load exactly when it hurts, and on node-3 it produced about 90 heartbeat triggers a second.

Requested:

- Back off exponentially between retries, bounded by `write_lease_wait`.
- Coalesce heartbeat triggers so concurrent writers share one.
- On giving up, return a distinct `ClientError::LeaderLeaseExpired { since }`, rather than a generic forward-to-leader, so callers can tell "no leader right now" from "ask someone else".

## Secondary observation, for investigation

node-2 streamed a snapshot to a follower about once a second, counter beyond 570, for at least 30 minutes (`openraft::replication::snapshot_transmitter: "569-th snapshot sending: has read snapshot: meta:{last_log:498.15760345, ...}"`). Meanwhile snapshots were built and the log purged every 5,000 entries (`purge log, last_purged: 498.15759268, purge_upto: 498.15764268`). Amafu writes about 160 entries a second, so a follower more than about 30 s behind can only catch up by snapshot, and may never catch up if each snapshot is superseded before it lands. We have not confirmed which follower or why. A metric for snapshots sent per peer, and a way to keep more log than one checkpoint interval, would help.

## Compatibility

All additions are opt-in. With no new builder calls, behaviour and defaults stay as they are today.

## How Amafu will use this

1. Before a planned reboot: wait for stable leadership (feature 4), then `shutdown_gracefully` (features 1 and 2), then reboot.
2. At startup: set ladder and RPC timeout for its environment (feature 3), and refuse to start if `timings().worst_case_failover` does not fit inside its fencing lease.
3. Treat `LeaderLeaseExpired` (feature 5) as "renew again shortly", not as an error.

## Code references at `b0a97b0`

- `crates/quorium/src/builder.rs`: `ladder: LadderConfig` is `pub(crate)` with no setter; `heartbeat` and `handoff_suspend` are the only timing setters.
- `crates/quorium/src/node.rs` (`start`): `ladder_for(b.heartbeat, &b.ladder)` then `raft_config(b.heartbeat, &ladder_config)`.
- `crates/quorium-core/src/config.rs`: `ladder_for` widens only `base`; `raft_config` clamps the heartbeat to `base / 3` and sets the election timeout to `2 × heartbeat`.
- `crates/quorium-core/src/ladder.rs`: `LadderConfig::default()`; `campaign_timeout()`, `max_campaign()` and `step()` scale with the measured consensus round trip; `TRANSFER_IDLE` = 2 s; `transfer_ready`.
- `crates/quorium-net/src/raft.rs`: `MIN_RPC_TTL` = 1 s applied to consensus RPCs.
- `crates/quorium/src/service.rs`: `LEASE_WAIT` = 2 s write loop with `trigger().heartbeat()` and a 10 ms sleep.
- `crates/quorium/src/node.rs`: `rebalance` transfers leadership only after `TRANSFER_IDLE` without writes; `shutdown` aborts tasks, shuts each raft down and closes the WAL without handing off.
