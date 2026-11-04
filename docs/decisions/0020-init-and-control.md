# 0020: A small init supervises one control process

Status: accepted for milestone 4 (replaces the single-process stand-in of 0005)

## Decision

- **PID 1 is `amafu-node` as init.** It mounts the file systems, loads kernel modules, brings up the underlay,
  reaps every orphan, owns the kernel watchdog and starts `amafu-node control`, restarting it with a backoff from 1
  to 30 seconds. It runs nothing else.
- **The control process runs every role**: Quorium on core members, the API, the controllers, the datapath, local
  services and the migration relay. Before its runtime starts it keeps only the capabilities it uses (chown, the DAC
  overrides, fowner, fsetid, kill, setuid, setgid, the four network ones, sys-admin for mounts, cgroups and block
  devices, and sys-resource) in its effective, permitted and bounding sets. Without `CAP_SYS_PTRACE` it cannot read
  a VM process's memory, and the tools it runs inherit no more than it holds.
- **The watchdog follows the control process's lease.** Control sends `feed` on `/run/amafu/watchdog.socket` while
  its lease is valid; init feeds the kernel watchdog on each message. A control process that dies or loses its lease
  stops the feeding, so the node resets one watchdog interval later unless a restarted control process holds a lease
  again by then.
- **VM processes survive a control restart.** They are children of control while it runs and of init after; the
  new control process finds them through their directories, as before.

## Consequences

- One binary still serves every role, chosen by the command (`control`, `vm-process`, or none as PID 1).
- `amafu-node run --configuration` keeps everything in one process without a watchdog, for development.
