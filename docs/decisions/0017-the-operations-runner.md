# 0017: Operations run as recorded, idempotent steps

Status: accepted for milestone 4

## Decision

- **An `Operation` is a cluster-wide resource**: its kind (migration, recovery, evacuation), its target (a node or a
  VM) and the full list of steps, fixed when it is created so it can be audited in advance. Admission refuses steps
  that are not exactly those of the kind. Only `is_paused` and `is_cancelled` may change afterwards, and a
  cancellation cannot be withdrawn.
- **The status records progress**: completed steps, the current step (attempts, last error, start time), a
  rollback in progress and the outcome (succeeded, failed with the reason, cancelled).
- **A step answers `is_complete`, `run` and `undo`.** `reconciler::advance` checks `is_complete` before running a
  step and again after it, then records completion with the runner's fencing token. A crash before the record is
  harmless because the next attempt sees the effect already holds; a crash after it moves on to the next step.
- **Failures roll back only while every touched step can be undone.** A permanent failure or a cancellation undoes
  the touched steps in reverse order and ends the operation failed or cancelled. Steps past a point of no return
  (marking a node lost, revoking it on storage, switching a VM over, releasing assignments) cannot be undone; an
  operation that fails there keeps retrying the step and shows the error, so an operator sees it and nothing unsafe
  happens.
- **The `operations` singleton** advances every unfinished operation, up to sixteen steps per pass, and revisits
  every 250 ms while steps wait on nodes. A finished operation is removed only when deleted.
- **Node-side work stays in the node's controllers.** Steps write intent (a VM's migration phase, a lease marked
  lost) and wait for nodes to report; they never call into a node directly.

## Consequences

- The simulator proves resumption by crashing between every pair of steps of an evacuation and by making a step's
  effect land just before a crash (`test-rig/simulated-world/tests/operations.rs`).
- Milestone 5's install and upgrade become new operation kinds with their own steps on the same runner.
