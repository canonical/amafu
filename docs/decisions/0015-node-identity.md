# 0015: Node certificates, revocation and rogue nodes

Status: accepted for milestone 3

## Decision

- **A node certificate names its node and roles** as URIs in its subject alternative names
  (`urn:amafu:node:<name>`, `urn:amafu:role:<role>`), with client and server use, and `amafu-state` as a DNS name for
  Quorium. Until install exists (milestone 5) the rig issues them from the lab's certificate authority and writes
  them into the node configuration's `identity` section.
- **The API's TLS listener accepts any client certificate, or none**, and proves only that the client holds the key.
  The node API then verifies the chain against the cluster authority, reads the node's name and roles, and checks the
  certificate's SHA-256 fingerprint against `CertificateRevocation` objects, on every request and again on every
  change of a subscription. Doing this after the handshake lets every refusal be audited, which a handshake failure
  could not be. User requests ignore client certificates.
- **`CertificateRevocation` is an admin-only kind.** Revoking a node is an admin decision; operators and tenants do not
  see revocations.
- **The rogue-node scenarios** try to subscribe, register a node and list VMs without a certificate, with another
  cluster's certificate and with a revoked one, and check that every attempt is refused and audited. A valid node
  that reaches outside its share (another node's object, a role its certificate lacks, a VM) is refused too.

## Consequences

- Quorium checks that a voter's certificate comes from the cluster authority but knows nothing of Amafu's
  revocations, so a revoked core member could still rejoin the voters. This is an upstream gap; it closes when the
  cluster issues and rotates certificates itself (milestone 5).
