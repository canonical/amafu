# 0013: Overlay networks and floating IPs

Status: accepted for milestone 3 (extends 0008)

## Decision

- **Every ready network gets a segment identifier** (the VXLAN network identifier), the lowest free one, allocated by
  the network singleton and kept in its status.
- **The hosting node publishes a `NetworkEndpoint` for each port** once the port is realized: project, network,
  VM, MAC, address, security groups, node and the node's underlay address. Endpoints are written only by nodes; users
  may read them in their project.
- **One VXLAN device per segment** (`amx` + hash of the segment and its identifier), enslaved to the segment's
  bridge, with learning off and ARP proxy on. For every remote endpoint on a local segment the node programs a static
  forwarding entry (MAC to the remote node's underlay address) and a neighbour entry (address to MAC). ARP is
  answered locally from those entries and nothing is flooded, so DHCP and gateway traffic never leave the node, and
  every node can run the same gateway address. DHCP offers an MTU of 1450 on overlay segments.
- **Security groups that name a group** now resolve to the addresses of its members on every node, read from the
  endpoints. DNS answers the names of remote ports too.
- **Floating IPs** come from `ExternalNetwork` ranges on the underlay, which operators declare. The floating IP
  singleton allocates the address and binds it to the node and interface of its target VM. That node adds the
  address as a /32 on its underlay interface, translates it to the private address (DNAT in, SNAT out, ahead of
  masquerading), lets the translated traffic through the port's ingress rules, and announces the address with
  gratuitous ARP. Raw frames are sent with `pnet_datalink`, which offers them without `unsafe` in Amafu.
- **Outbound access** now leaves through whichever interface routes it, not only the underlay, because lab nodes have
  a management interface that holds the default route.

## Consequences

- During a move, the old node keeps the address until its next convergence, so for a moment both nodes answer. The
  gratuitous ARP from the new node decides where traffic goes, and the floating IP scenario checks that only the new
  node serves the address once the move settles. It is therefore not one of the global invariants.
- BGP announcement of floating IPs is tested in the MAAS lab against the FRR router, which milestone 3 defers with
  the MAAS lab (0005).
- The kernel datapath owns every link whose name starts with `amb`, `amt` or `amx`.
