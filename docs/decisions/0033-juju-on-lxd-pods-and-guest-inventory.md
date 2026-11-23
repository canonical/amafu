# 0033: Juju runs on LXD pods; Amafu sees what VMs host

Status: accepted for milestone 7; supersedes the VM substrate of 0028 and the bootstrap of 0032

## Context

Juju controllers on VMs used Juju's manual provider: Amafu created a VM for every controller and every unit, registered it with Juju, and ran Juju's provisioning script in it. Each step needed code in Amafu that Juju already has for real clouds, and that code is where the lab found most of its bugs. Users also run LXD inside their own VMs, and Amafu could not show what runs there.

## Decision

- A Juju controller runs on pods: 1, or 3 to 16, VMs that form an LXD cluster. Juju bootstraps onto that cluster as its `localhost` LXD cloud and sees each pod as a zone. Its controller becomes a container, and Juju creates and removes unit containers itself. Amafu creates, scales and retires pods, and drives Juju only through its API: deploy, scale, remove units and applications, expose, enable HA.
- Two pods are refused: with two members the LXD database has one voter, and the lab lost its cluster after a rollout moved them.
- Containers sit directly on the pods' own Amafu network. Each pod bridges its uplink (`br0`, keeping the VM's MAC) and LXD's default profile puts every instance on that bridge. Macvlan was the first choice but it isolates a host from its own containers, so the Juju client on the founding pod could not reach the controller it had just created.
- A VM interface can host up to 64 guests; pods ask for 32. Allocation reserves an address pool for the interface, the node's DHCP answers new LXD MACs (00:16:3e) seen on it with a pool address, the node network controller records each lease as a guest endpoint (renewed at most every 15 minutes, gone after 2 hours unrenewed), and other nodes reach guests through those endpoints like any port. Guests share their pod's security groups.
- Amafu reaches the founding pod's LXD API through a floating IP, as it reaches Kubernetes and Juju APIs: the step runner may run on a node with no port on the pods' network. Through it Amafu enables clustering, issues a join token per new pod and removes retired pods from the cluster. Each new pod joins at first boot with `lxd init --preseed`, the token in its user data; Amafu never calls the other pods.
- The founding pod bootstraps Juju itself once every pod is online, with the controller's CA and admin password in its user data as before. Amafu finds the controller container in the founding pods' guest inventories (LXD marks it `user.juju-is-controller`) and points the controller's API floating IP at that container's address.
- Amafu has one LXD client identity, LxdIdentity, readable by admins and nodes; its certificate is public so users can trust it on their own VMs.
- Each node probes the running VMs it hosts for LXD on port 8443, lists their instances when trusted, and writes a GuestInventory per VM: none, untrusted, or trusted with the cluster member and, for the instances that member runs, their type, status, IPv4 addresses and whether they are a Juju controller. The dashboard expands such VMs, and the pods of a controller, into lxc and vm pills.
- Scaling pods down only retires pods that run no instances; Amafu refuses rather than moving units behind Juju's back.
- Juju controllers on the old VM substrate cannot be converted or read. Delete them before installing this release. Their change records stay behind and break listing until deleted; a delete of any stored object the release cannot read erases it outright, since no controller could finalize it.

## Consequences

- The manual provider code is gone: controller and unit machine steps, provisioning scripts, manual machines and their hardware, the bootstrap SSH key, machine replacement, and the per-machine security groups and labels.
- Each controller on pods holds two floating IPs, one for the Juju API and one for the founding pod's LXD API; the pods' security group opens 17070 and 8443, and LXD only acts for trusted certificates.
- Join tokens live in pod user data, which project members can read; they are single-use and expire after three hours.
- LXD servers present self-signed certificates, which Amafu accepts as it does for k8sd.
- Unit VMs need nested virtualization and are not offered yet; units are containers.
