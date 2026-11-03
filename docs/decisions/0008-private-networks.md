# 0008: Private networks on the node

Status: accepted for milestone 2

## Decision

- **Addresses are allocated at placement.** The placement controller assigns each interface an address from its
  network together with the node, in one status write, so an address is never handed out twice even if two nodes
  start VMs at once. The gateway is the subnet's first address.
- **Subnets may not overlap**, across every project. A network whose subnet overlaps a ready one fails with the
  reason. Every network's gateway lives in the node's routing table, so overlapping subnets would be ambiguous there.
- **One Linux bridge per network** (`amb` + hash), one persistent tap per interface (`amt` + hash), created over
  netlink. The bridge holds the gateway address.
- **Policy in nftables**, compiled from the desired state into one ruleset and applied atomically with the `nft`
  binary (`nft --file -`), which is simpler and better tested than speaking the nftables netlink protocol. The bridge
  table drops frames whose source MAC, IP or ARP address is not the interface's own, and runs each interface's
  security group chains; ARP crosses the bridge before those chains. The inet table lets guests reach only DHCP, DNS,
  ICMP and metadata on the node, and masquerades outbound traffic when the network allows it.
- **Security groups**: ingress is denied unless a rule allows it; egress is allowed unless the group has egress
  rules. A rule's remote is any address, a CIDR, or another group, which resolves to its members' addresses.
- **Local services** on each gateway: DHCP answers only the interface's assigned address, DNS answers
  `<vm>.<network>.<project>.internal` and forwards the rest to the configured resolvers, and a NoCloud metadata
  server on 169.254.169.254 serves meta-data and user-data to the address that asks. Their parsers are fuzzed by
  `crates/network/fuzz`.

## Consequences

- A guest that forges its address is cut off, which the security-group scenario checks in the lab.
- Networks span one node in milestone 2. Overlay networking between nodes is milestone 3.
