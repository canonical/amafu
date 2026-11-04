# 0019: Live migration and evacuation

Status: accepted for milestone 4

## Decision

- **A migration is an operation of five steps.** Prepare the destination (the scheduler chooses one when none is
  named; the VM's status gains a migration record with the attempt, source, destination and its underlay address,
  and a downtime target of 100 ms), copy memory, switch over (one fenced write moves `status.node`), announce the
  addresses (complete when the VM's network endpoints and floating IPs name the destination) and clean up the
  source. A failure before the switch-over undoes the preparation and leaves the VM on its source.
- **Nodes do their half through the slice.** The destination takes part in the migration (it receives the VM, its
  volumes and project endpoints, may write only the migration record, and gets ports but publishes no endpoint
  until the switch-over). It starts a VM process that waits for the VM and reports `destination-ready`. The source
  sends when the phase is `copying`, reports `copied` with the report, and after the switch-over stops what is
  left, releases its volumes and reports `cleaned`. A VM process that is receiving does not count as running.
- **The VM process never opens a network socket.** Cloud Hypervisor sends over a unix socket in the VM directory;
  the node relays it to the destination node's port 7445 over mutual TLS with node certificates, verifying the
  destination's name, and the destination relays it into the waiting VMM's socket. The VMM's own API is a unix
  socket in the same directory.
- **The two VMs must agree on paths and descriptors.** Disks are opened through `disks/<index>` links in the VM
  directory, which exists under the same path on every node, and taps are moved to descriptors 200 and up, because
  Cloud Hypervisor's receiver takes the configuration from the sender.
- **Slow migrations are throttled**: after 10 s the source's cgroup CPU quota drops to 50%, then 25%, 10% and 5% of
  its vCPUs every 5 s. The report (bytes, final round, duration and downtime) comes from the VMM's own log.
- **Evacuation cordons the node, then runs one migration per VM** listed when the evacuation was created.
- VMs with a local disk volume cannot move; the destination choice says so.

## Consequences

- Cloud Hypervisor sends over one connection; parallel streams and zero-page skipping are left to upstream.
- Cancelling during the copy undoes the preparation; if the copy had already finished, the source restarts the VM
  cold. Cancelling after the switch-over is refused.
- An evacuation that cannot move a VM keeps retrying it rather than rolling back the cordon.
