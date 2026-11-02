# 0002: Store state on a single-voter Quorium node behind the Store trait

Status: accepted for milestone 1, to be revisited in milestone 3

## Context

Milestone 1 needs a real, persistent, consensus-backed store behind the `Store` trait, with a single voter. The
spike looked at Quorium at commit `b0a97b08a9bffebf0c977692f5ac5dd64dd9fd7a` (version `0.1.0-alpha.1`, pinned to
openraft `0.10.0-alpha.37`).

What Quorium offers that Amafu needs:

- Typed entities whose write methods run on every replica in log order, so compare-and-swap can be a replicated
  write with no read-modify-write race.
- Linearizable reads, and stale reads that keep working without a majority.
- A per-entity `watch()` that reports changes in commit order and skips to the latest state for slow consumers.
- Singletons with fencing tokens, for the scheduler in milestone 3.
- `single_node_dev_mode()`, which starts a one-voter cluster. Outside that mode the builder accepts only 3 or 5
  voters.

What it lacks:

- No listing or range scan over entities.
- No watch over a set of entities; only one entity at a time.
- No transactions across entities.
- Every entity is held in memory and should stay under 1 MiB.
- Its testing simulator never fires `watch()`, so it cannot drive Amafu's controllers.

## Decision

`amafu-state` implements `Store` with three kinds of Quorium entity:

- `amafu.object`, keyed by kind, project and name, holds the JSON of one object and a revision. Its only write is
  `compare_and_swap(expected_revision, replacement)`. The revision of a key never decreases, even across removal
  and re-creation. All generation and status rules stay in `domain::apply_change`, applied by
  `interfaces::objects` before the swap, so the store and the in-memory fake share one rule set.
- `amafu.kind-catalog`, one per kind, holds the members of the kind and the last 256 changes with a sequence
  number. Creating an object adds the member before the object is written; removing it removes the member
  afterwards. Every write then appends to the change feed. A list reads the catalog and then each member, skipping
  members whose object does not exist.
- `amafu.audit-cursor` and `amafu.audit-segment` hold the audit log as numbered segments of 256 entries.

A kind watch follows the catalog entity. It emits `Changed` for every change after the last sequence it saw. When it
cannot tell what it missed (the first notification, or a gap longer than the feed), it emits `Resynchronize`
followed by every change still in the feed. Watches therefore notify at least once, never at most once. Controllers
already treat `Resynchronize` as a reason to rediscover their keys, and they resynchronize periodically anyway.

The node starts one voter with `single_node_dev_mode()` in plaintext on a loopback address. Only the node's own
roles reach it.

## Consequences

- An object write costs two Quorium writes (three when the object is created). A crash between the object write
  and the catalog write can lose a change notification, or leave a member whose object does not exist. The first
  is repaired by periodic resynchronization; the second is skipped by `list` and never removed. This is acceptable
  for milestone 1 and is covered by the store contract suite, which the fake and the Quorium store both pass.
- Every write of one kind goes through one catalog entity, which serializes writes per kind and bounds a kind to
  roughly twenty thousand objects (the 1 MiB entity guidance). Milestone 3 must shard the catalog (by project, or by
  a hash of the name) before the scheduler and many nodes raise the write rate.
- Finding an audit entry scans the newest 16 segments (4,096 entries). The audit export in milestone 6 replaces
  this with a proper index.
- The single voter runs in Quorium's development mode, without TLS. Milestone 3 replaces it with three voters
  over mutual TLS. The voter cap with non-voting members, client-side watch streams and forced recovery from a
  minority, which the architecture lists as Quorium gaps, are still missing and remain upstream work for
  milestone 3.
- Quorium and openraft are pre-releases with an unstable API, so the dependency is pinned to an exact commit and
  every use stays inside `amafu-state`.

## Found by the virtual lab: Quorium cannot recover a log with a zero-filled tail

The first forced power cycle in the virtual lab left the node unable to start (`wal corrupt at byte 321:
undecodable record`). After power loss, ext4 can leave the end of the write-ahead log filled with zeros. Quorium's
frame reader treats a zero-length frame as a valid record, because the CRC32 of an empty payload is zero, and then
fails to decode it. A zero-length frame can never be a real record, so the reader should treat it as the torn tail
it is.

Until that is fixed upstream, `amafu-state` repairs the log before Quorium opens it: it walks the frames in Quorium's
format and, only if every byte after the last valid frame is zero, truncates the file there. Any other damage is
left for Quorium to report. `crates/state/tests/reopening.rs` reproduces the failure. The fix belongs upstream (one
line in `quorium-store`'s `frame` function); the workaround is removed when the pinned commit includes it.
