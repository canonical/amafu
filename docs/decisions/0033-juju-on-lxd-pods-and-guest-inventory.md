# 0033: Juju runs on LXD pods; Amafu sees what VMs host

Status: accepted for milestone 7; supersedes the VM substrate of 0028 and the bootstrap of 0032

## Context

Juju controllers on VMs used Juju's manual provider: Amafu created a VM for every controller and every unit, registered it with Juju, and ran Juju's provisioning script in it. Each step needed code in Amafu that Juju already has for real clouds, and that code is where the lab found most of its bugs. Users also run LXD inside their own VMs, and Amafu could not show what runs there.

## Decision

- A Juju controller on VMs runs on pods: a few VMs that form an LXD cluster. Juju bootstraps onto that cluster as an LXD cloud, its controller becomes a container, and Juju creates and removes unit containers itself. Amafu creates, scales and removes pods, and drives Juju only through its API.
- Containers sit on the pods' own Amafu network through macvlan. A VM interface can host up to 64 guests: allocation reserves an address pool for it, the node's DHCP answers new LXD MACs (00:16:3e) seen on that interface with a pool address, the node network controller records each lease as a guest endpoint (renewed at most every 15 minutes, gone after 2 hours unrenewed), and other nodes reach guests through those endpoints like any port. Guests share their pod's security groups.
- Amafu has one LXD client identity, LxdIdentity, readable by admins and nodes; its certificate is public so users can trust it on their own VMs.
- Each node probes the running VMs it hosts for LXD on port 8443, lists their instances when trusted, and writes a GuestInventory per VM: none, untrusted, or trusted with the cluster member and the instances with their type and status. The dashboard expands such VMs into lxc and vm pills.
- Juju controllers on the old VM substrate cannot be converted; the release refuses to upgrade while one exists, and admission refuses the old shape.

## Consequences

- The manual provider code goes: controller and unit machine steps, provisioning scripts, manual machines and their hardware, the bootstrap SSH key, and the per-machine security groups and labels.
- LXD servers present self-signed certificates, which Amafu accepts as it does for k8sd.
- Unit VMs need nested virtualization and are not offered yet; units are containers.
