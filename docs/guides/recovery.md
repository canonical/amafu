# Recovering from failures

## A node fails

Nothing needs doing. Every node holds a lease that it renews through the core members, and a watchdog resets a node
that cannot renew it (decision 0018). When a lease expires, the cluster starts a `recovery` operation that marks the
node lost, revokes it on every storage backend, waits out its watchdog, releases its VMs and lets the scheduler
place them elsewhere by priority. With the defaults, a powered-off node's VMs run elsewhere about 20 seconds later.

```sh
amafu operation list                 # recovery-<node>-<epoch>
amafu node show <node>
```

If revoking the node on a storage backend fails, the recovery stays on that step and retries: Amafu prefers
downtime to two writers. Fix the backend's reachability, or check its credentials, and the recovery continues.

When the node comes back it starts with nothing running and rejoins. To keep work off it while you investigate:

```sh
amafu node cordon <node>
amafu node uncordon <node>
```

## Taking a node out for maintenance

```sh
amafu node evacuate <node> --wait     # cordons it and live-migrates every VM off it
# ... maintenance ...
amafu node uncordon <node>
```

## A core member fails

With three core members the cluster keeps working while any one of them is down: the API, the dashboard and
every controller move to the others. A restarted core member serves again once it has caught up, about 20 seconds
after it starts. While two of the three are down, the cluster cannot accept changes, and nodes cannot renew their
leases, which are written to the same store: after the lease and watchdog intervals they reset themselves, as a
cut-off node does. Bring a core member back first.

Core members are fixed at install. Replacing one that is lost for good is not supported in this release; bring the
same machine back, with its disk, under the same name.

## A storage backend fails

Volumes on an unreachable backend cannot be created, attached or migrated, and the dashboard shows the backend as
unreachable. Running VMs keep their connections as long as the backend serves them. Recoveries that need to revoke a
node on that backend wait for it.

## An operation fails

```sh
amafu operation show <name>          # the failing step and its last error
amafu operation cancel <name>        # undoes it if no step is past undoing
amafu operation delete <name>        # once it has finished
```

## An upgrade goes wrong

A node that cannot confirm the new image boots its previous slot again by itself, and the upgrade stops there. Roll
the cluster back with `amafu upgrade rollback` (see [upgrading](upgrade.md)), unless it has been finalized.

## The dashboard is unreachable

Dashboard instances hold no state; sessions are sealed in the browser's cookie with a key all instances share. While
any instance runs, the virtual address moves to it and signed-in users keep their sessions. The command line talks
to the cluster directly and does not need the dashboard.

## Audit export falls behind

```sh
amafu audit-sink list
```

A sink whose destination refuses entries shows the reason and retries; it resumes from where it stopped, and may
deliver the last batch twice. Every entry carries its sequence number.
