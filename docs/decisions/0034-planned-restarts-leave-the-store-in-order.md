# 0034: Planned restarts leave the store in order

Status: accepted for milestone 7

## Context

Rolling upgrades on the lab reset the whole cluster when one node rebooted. Recorded consoles showed every reboot itself was clean. The failure was in the store: while one voter wrote its boot slot and rebooted, the two others stalled past the one-second leader lease and RPC timeout, could not keep or re-elect a leader for more than six seconds, and so could not renew their eight-second node leases. Their watchdogs reset them. A node reset while trying a new image was then counted as "could not boot", which blocked that version and forced a new release number.

quorium 0.1.0-alpha.2 added what Amafu needed to prevent this: leadership handoff, graceful shutdown, configurable timing with a failover estimate, and leadership statistics.

## Decision

- The store runs with a 500 ms heartbeat and a 2 s leader lease and RPC timeout, configurable in the node configuration, and keeps 50,000 log entries behind each checkpoint so a lagging follower catches up from the log instead of by snapshot.
- A core node refuses to start when quorium's estimate of the time to elect a new leader is longer than its node lease minus one renewal interval: a failover must always finish before a healthy node's lease runs out.
- A node only reboots into a new image once its store has had a stable leader, without elections, for 30 seconds.
- A planned reboot leaves in order: hand leadership to a caught-up voter and ask the others to pass this node over while it restarts, surrender the node lease, close the store cleanly, then reboot. If the handoff fails, the node does not reboot and tries again later.
- A trial boot that reached the new image and was then reset is retried, up to three attempts per image. Only a trial that never booted, ran out of attempts or was given up on counts as a fall back.
- Each core node reports its store health (stable or not, elections, leader changes, time without a leader) in its status, shown by `amafu node list` and the dashboard, and the lab release script refuses to start while any node's store is unsettled.

## Consequences

- Upgrades wait for a calm store before each reboot, so a struggling cluster delays its upgrade instead of losing nodes.
- While a leader hands off, writes to its shard wait at most a few seconds; quorium holds them for the whole transfer, which is bounded by the four-second handoff deadline.
- A node that can no longer start because its timing does not fit its lease reports the conflict instead of joining a cluster that would fence itself.
- Nodes that are not voters report no store health and have nothing to hand off.
