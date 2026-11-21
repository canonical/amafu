# 0029: Cloud Hypervisor v55, configurations built from safe parts, and taps handed over the API socket

Status: accepted for milestone 7; amends 0001

## Context

On Cloud Hypervisor v54, a VM that is received in a live migration cannot be given its tap devices: the
receiving VMM fails with `VnetReservedFd`. v55 adds `external_fds` to the receive request for exactly this, but
the field is crate-private, so an embedder can fill it only through the VMM's HTTP API, where descriptors arrive
as `SCM_RIGHTS` with the request. v55 also made `VmConfig::parse`, which 0001 relies on, an `unsafe fn`, because it
takes descriptor numbers from strings.

## Decision

- Amafu embeds Cloud Hypervisor v55.
- `vm-process` builds `VmConfig` without `VmConfig::parse`: each part comes from Cloud Hypervisor's own safe
  parsers (`CpusConfig`, `MemoryConfig`, `DiskConfig` and the others) or from a small JSON document, serde fills
  the defaults, and taps are attached as `ExternalFd`s taken from owned descriptors. The VMM's `validate` runs as
  the parser's did.
- Interfaces get stable device identifiers, `net0`, `net1` and on in plan order, so a receiving VMM can name the
  devices it needs descriptors for.
- Booting still goes through the in-process request channel. Receiving connects to the VMM's own API socket and
  sends `PUT /api/v1/vm.receive-migration`, with one `external_fds` entry per interface, and the taps as
  `SCM_RIGHTS` in the same `sendmsg`. The request returns when the VM has arrived.
- Requests to the VMM API, for sending and receiving, share one framing module.

## Consequences

- VMs with network interfaces live-migrate. On the lab, a VM moved between nodes keeps its boot, its files and its
  floating IP, and its SSH port is unreachable for about a second during switch-over.
- A VM booted before this change has automatically named devices and cannot be received this way; restarting it
  once gives it stable names.
- The VMM's API socket stays open for the life of each VM process. It lives in the VM's runtime directory, which
  only the VM's user and the node can reach.
- Every Cloud Hypervisor upgrade must still compare lockfiles, as 0001 says, and now also check that the receive
  request's `external_fds` format is unchanged.
