# 0004: What the four fixed roles may do

Status: accepted

## Context

The plan fixes four roles, admin, operator, member and viewer, with no custom roles, and requires an authorization
matrix that is tested exhaustively for every role, verb and kind. It does not say what each role may do. Every user
identity is bound to one project.

## Decision

| Role | Project-scoped kinds in its own project | NetworkEndpoint in its own project | in another project | Node | StorageBackend, ExternalNetwork | CertificateRevocation |
|---|---|---|---|---|---|---|
| admin | every verb | every verb | every verb | every verb | every verb | every verb |
| operator | every verb | get, list, watch | nothing | get, list, watch, update | every verb | nothing |
| member | every verb | get, list, watch | nothing | nothing | get, list, watch | nothing |
| viewer | get, list, watch | get, list, watch | nothing | nothing | get, list, watch | nothing |

The project-scoped kinds users manage are Vm, Image, Volume, Network, SecurityGroup and FloatingIp. Network
endpoints are project-scoped but *managed by nodes*: the hosting node publishes them and users may only read them.
Cluster-wide kinds carry a policy: nodes are *managed by nodes* (they register themselves), storage backends and
external networks are *shared infrastructure* that operators manage and tenants may see, so a member can name them,
and certificate revocations are *admin only*. Nodes themselves are authorized by their share of the state, not by
these roles (0011).

- Listing a kind across every project (`?all-projects=true`) is reserved to admins.
- Operators manage the infrastructure (cordoning nodes) and their own project's workloads. They cannot create or
  delete nodes: nodes register themselves, and removing one is an admin decision.
- Members and viewers are tenants: they do not see the nodes, but they see the storage backends they may use.
- A denial is answered with 403 before the object is looked up, so a denial reveals nothing about what exists.

The implementation is `amafu_api::authorization::is_allowed`. The expectation the authorization scenario checks
against is a separate table in `test-rig/checkers/src/authorization_policy.rs`, written from this record, so that a
bug in the implementation cannot also make the expectation wrong.

## Consequences

- A new kind must be added to the expectation table; the checkers' own test fails until it is.
- In milestone 1, users authenticate with bearer tokens stored as SHA-256 digests in the node configuration. The
  architecture's mutual TLS client certificates and OIDC tokens replace them in a later milestone without changing
  this table, because authorization only sees a principal with a name, a role and a project.
