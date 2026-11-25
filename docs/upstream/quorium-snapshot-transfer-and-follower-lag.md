# Bug and feature request: snapshots larger than 16 MiB never reach a follower, and follower lag is invisible

Against quorium `0.1.0-alpha.2` (openraft `0.10.0-alpha.37`), TCP transport, QUIC disabled.

## Summary

A follower that has fallen behind the leader's retained log can only catch up by installing a snapshot. quorium sends that snapshot as a single framed message, and the TCP codec refuses any frame larger than 16 MiB. Once a store's state machine is larger than that, a lagging follower can never rejoin: the leader retries about once a second, forever. Nothing is logged at the default level, and `Stats::rpc_failures` stays at zero.

The failure is silent, and quorium exposes nothing about a follower's progress. So an application can't tell that its three-voter cluster really runs on two voters until it restarts one of them and loses quorum.

We ask for:

1. Snapshots of any size can be transferred (bug).
2. Snapshot transfer failures are logged and counted.
3. Per-follower replication progress is exposed.

## What we observed

In Amafu's lab, a three-voter cluster with a state machine of about 68 MB (mostly an append-only audit log):

- One voter was frozen at log index 10,598,248 while the leader was past 16,340,316.
- The leader logged `N-th snapshot sending` more than a hundred times per snapshot, about once a second. `stats().snapshots_sent` for that follower kept rising, and nothing else showed a problem.
- The lagging voter still reported a known leader, so `is_stable(window)` was true on every node.
- Restarting either healthy voter left no quorum. Writes stopped for the whole restart, and every node's application-level lease lapsed.
- Wiping the lagging voter's data made no difference. It rejoined as an empty learner, received 385 snapshot attempts in a few minutes, and stayed empty.

## Reproduction

Three voters over mutual TLS, `default-features = false`:

1. Write 25,000 entities of about 1.5 KB each, for a state machine of about 40 MB, with `log_retention` at 50,000 entries.
2. Stop one voter, delete its `wal` and `state.redb`, write 200 more entities, and start it again.
3. The restarted voter never reads any of the entities. The leader's `stats().snapshots_sent` for it reached 81 in 90 seconds.

The same steps with 20,000 entities of 200 bytes each work, because the follower catches up from the log.

## Cause

- `quorium-net/src/codec.rs`: `MAX_FRAME` is 16 MiB, and `encode_frame` returns `CodecError::FrameTooLarge` for a larger body. `FrameDecoder::next_frame` refuses the same.
- `quorium-net/src/raft.rs`: `PeerConn::full_snapshot` sends the whole snapshot (`SnapshotData = Vec<u8>`) as one request through `call(&clients.snapshots, …)`.
- openraft only logs `stream_snapshot: backoff timeout` at debug level. The error that caused the retry is not logged, and `rpc_failures` does not count it.
- The QUIC transport allows 64 MiB per message (`quic.rs`, `MAX_MESSAGE`). That only moves the ceiling.

## Requested changes

### 1. Transfer snapshots of any size

Stream a snapshot in bounded chunks, for example 1 to 4 MiB, each within the frame limit. The follower assembles them, verifies the result, then installs it. Chunks should resume from the last acknowledged offset after a transient failure, rather than restarting a large transfer every second. A transfer should have its own deadline, scaled to its size, separate from the consensus RPC timeout.

### 2. Make snapshot failures visible

- Log every failed snapshot send at warn level, with the follower, the snapshot size and the error.
- Count failures separately from attempts, for example `Stats::snapshot_failures: BTreeMap<NodeId, u64>`, and include them in `rpc_failures`.

### 3. Expose each follower's replication progress

On the leader, for example:

```rust
pub struct FollowerProgress {
    pub id: NodeId,
    pub is_voter: bool,
    pub matched_index: Option<u64>,
    pub entries_behind: u64,
    pub last_contact: Option<Duration>,
    pub receiving_snapshot: bool,
}

impl Quorium {
    pub fn followers(&self) -> Option<Vec<FollowerProgress>>;
    pub fn voters_caught_up(&self, slack: u64) -> Option<bool>;
}
```

`None` when this node is not the leader. openraft already tracks `RaftMetrics::replication`, and `rebalance` uses it (`CAUGHT_UP_SLACK`) to decide when a learner may be promoted. Exposing it would let an application refuse a planned restart whenever it would leave the cluster without a caught-up quorum.

On every node, an equivalent local signal would help too, for example the index of the last applied entry next to the leader's last known commit index. Then a follower can report that it is behind without asking the leader.

## Secondary observation: a debug assertion in openraft

In the reproduction, a debug build panicked in the leader's replication progress (`openraft/src/progress/entry/update.rs:104`) once the wiped voter restarted: "follower log reversion is not allowed without `allow_log_reversion` enabled; matching: 1.75001; conflict: 75001". After that, the panicked task answered every RPC with `Failed("panicked")`.

A voter that loses its disk and restarts with the same node id is a normal operational event. quorium could either enable `allow_log_reversion` for it, or require such a node to rejoin under a new id.

## How Amafu works around it today

- Amafu keeps only the newest audit entries in the store, so its state machine stays well under 16 MiB.
- In its store health, each leader reports a follower whose `snapshots_sent` counter rose within the last 30 seconds as catching up. A node won't restart for an upgrade while any node reports one.

These replace neither a chunked transfer nor a real progress API. The workaround only notices a lagging follower after the leader has started sending it snapshots.
