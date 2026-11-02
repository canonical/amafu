# 0004: What the four fixed roles may do

Status: accepted

## Context

The plan fixes four roles, admin, operator, member and viewer, with no custom roles, and requires an authorization
matrix that is tested exhaustively for every role, verb and kind. It does not say what each role may do. Every user
identity is bound to one project.

## Decision

| Role | Project-scoped kinds (Vm) in its own project | in another project | Cluster-wide kinds (Node) |
|---|---|---|---|
| admin | every verb | every verb | every verb |
| operator | every verb | nothing | get, list, watch, update |
| member | every verb | nothing | nothing |
| viewer | get, list, watch | nothing | nothing |

- Listing a kind across every project (`?all-projects=true`) is reserved to admins.
- Operators manage the infrastructure (cordoning nodes) and their own project's workloads. They cannot create or
  delete nodes: nodes register themselves, and removing one is an admin decision.
- Members and viewers are tenants: they do not see the nodes.
- A denial is answered with 403 before the object is looked up, so a denial reveals nothing about what exists.

The implementation is `amafu_api::authorization::is_allowed`. The expectation the authorization scenario checks
against is a separate table in `test-rig/checkers/src/authorization_policy.rs`, written from this record, so that a
bug in the implementation cannot also make the expectation wrong.

## Consequences

- A new kind must be added to the expectation table; the checkers' own test fails until it is.
- In milestone 1, users authenticate with bearer tokens stored as SHA-256 digests in the node configuration. The
  architecture's mutual TLS client certificates and OIDC tokens replace them in a later milestone without changing
  this table, because authorization only sees a principal with a name, a role and a project.
