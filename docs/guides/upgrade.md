# Upgrading a cluster

An upgrade rolls a signed image across the cluster one wave at a time. Every node keeps two boot slots: the new image
is written to the inactive slot, the node reboots into it once, and it becomes the default only after the node has
stayed healthy. VMs live-migrate off each node before it reboots, so no workload restarts (decision 0021).

## Plan

```sh
amafu upgrade status                                        # which image each node runs, and from which slot
amafu upgrade start --image manifest.json --dry-run         # the steps, without starting them
```

The waves are a canary of one node, then each zone, then the core members one at a time. Each node is cordoned,
evacuated, written, rebooted, checked and uncordoned; after each wave the cluster is watched before the next one
starts. Tune it with:

| Option | Default | Meaning |
|---|---|---|
| `--canary-count` | 1 | Nodes upgraded alone before the rest |
| `--health-deadline-seconds` | 900 | How long a node has to come back healthy |
| `--soak-seconds` | 300 | How long to watch the cluster between waves |

The cluster needs room to move each node's VMs elsewhere; check the overview's capacity by zone first.

## Run

```sh
amafu upgrade start --image manifest.json --wait 14400
amafu operation show <name>          # progress, the current step and its last error
amafu operation pause <name>         # stops before the next step
amafu operation resume <name>
```

The upgrade is an operation like any other, so it survives the loss of any core member and shows on the dashboard's
Operations page. A node that does not confirm the new image falls back to its previous slot by itself, and the
operation stops at that node.

## Roll back

```sh
amafu upgrade rollback --wait 14400
```

A rollback is the same plan with the previous image and the waves reversed. It is possible until the cluster is
finalized.

## Finalize

```sh
amafu upgrade finalize
```

Finalizing enables the new image's features and forgets the previous image: after it, the cluster cannot roll back,
and nodes refuse any image below the new feature level. Finalize once the new release has run long enough for you to
trust it.
