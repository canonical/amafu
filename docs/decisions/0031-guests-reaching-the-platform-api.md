# 0031: Guests reach the platform API only on networks that ask for it

Status: accepted for milestone 7; amends 0013 and 0028

## Context

Kubernetes clusters and Juju controllers that Amafu runs on its own VMs validate Amafu's tokens: their API servers fetch Amafu's discovery document and signing keys, and the add-ons still to come (the cloud controller manager and the cluster API providers) call the Amafu API itself. Each node drops all guest traffic towards the infrastructure networks, so no guest could reach the API, and every token was refused.

## Decision

- A network opts in with `is_platform_api_allowed`, off by default. `amafu network allow-platform-api` and the Networks page switch it.
- On such networks, a guest may open TCP connections to the core members' API addresses and port, and nothing else on the infrastructure networks. The addresses come from each node's configuration: the state voters with the API port, or the core members' URLs.
- New connections are limited per guest address: 20 a second with a burst of 40, and 64 at once; beyond that they are dropped or reset.
- The packet is marked rather than accepted, so the guest's own security group egress rules still apply before it is let through. A guest calling its own node is admitted on the input path the same way.
- On a network without outbound access, only this marked traffic is translated to the node's address.
- Admission refuses a Kubernetes cluster or a Juju controller on VMs whose network does not allow it, and says how to allow it.

## Consequences

- Tenant VMs on an opted-in network can reach the API's unauthenticated endpoints (discovery, keys, token exchange) and present stolen tokens from inside. Authorization is unchanged.
- The API sees forwarded calls as coming from the node's address, so audit records the identity but not the VM. Calls to a guest's own node keep the guest's address.
- The API has no limit of its own yet; the per-guest limits bound one VM, not many.
