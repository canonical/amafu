import { MainTable, Notification } from "@canonical/react-components";

import { update } from "../cluster/client";
import type { Operation, OperationSpecification } from "../cluster/kinds";
import { useCollection } from "../cluster/live";
import { OPERATIONS } from "../cluster/operations";
import CollectionNotice from "../components/CollectionNotice";
import Command from "../components/Command";
import DeleteConfirmation from "../components/DeleteConfirmation";
import IconAction from "../components/IconAction";
import Meter from "../components/Meter";
import PageHeader from "../components/PageHeader";
import Panel from "../components/Panel";
import Status from "../components/Status";
import SubjectLink from "../components/SubjectLink";
import { usePanel } from "../components/panels";
import { useSubmission } from "../components/submission";
import { describeStep, sentence, sinceNow } from "../format";

export function operationPhase(operation: Operation): { phase: string; label?: string; detail?: string } {
  const outcome = operation.status.outcome;
  if (outcome) {
    return { phase: outcome.outcome, detail: outcome.outcome === "failed" ? outcome.reason : undefined };
  }
  if (operation.status.rollback) {
    return { phase: "stopping", label: "Rolling back", detail: `${operation.status.rollback.remaining_steps} steps to undo` };
  }
  if (operation.specification.is_paused) {
    return { phase: "paused" };
  }
  return { phase: "in-progress", label: "In progress", detail: operation.status.current_step?.last_error };
}

export function targetOf(operation: Operation): string {
  const target = operation.specification.target;
  if (target.kind === "node") {
    return target.node;
  }
  return target.kind === "vm" ? `${target.vm.project}/${target.vm.name}` : "the cluster";
}

export function currentStepOf(operation: Operation): string {
  const completed = operation.status.completed_steps ?? 0;
  const step = operation.specification.steps[completed];
  return operation.status.outcome || !step ? "" : describeStep(step);
}

function change(operation: Operation, edit: Partial<OperationSpecification>): Promise<unknown> {
  return update(OPERATIONS, operation.metadata.name, { ...operation.specification, ...edit }, operation.metadata.generation);
}

function OperationDetail({ operation, onClose }: { operation: Operation; onClose: () => void }) {
  const completed = operation.status.completed_steps ?? 0;
  return (
    <Panel title={operation.metadata.name} onClose={onClose}>
      <p>
        {sentence(operation.specification.kind)} of {targetOf(operation)}
      </p>
      <Status {...operationPhase(operation)} />
      <ol className="step-list">
        {operation.specification.steps.map((step, index) => {
          const state = index < completed ? "done" : index === completed && !operation.status.outcome ? "current" : "waiting";
          return (
            <li key={`${step.step}-${index}`} className={`step-list__step step-list__step--${state}`}>
              {describeStep(step)}
              {state === "current" && operation.status.current_step && (
                <span className="secondary-line">
                  {" "}
                  started {sinceNow(operation.status.current_step.started_at)}, attempt {operation.status.current_step.attempts}
                </span>
              )}
            </li>
          );
        })}
      </ol>
      <Command words={["amafu", "operation", "show", operation.metadata.name]} />
    </Panel>
  );
}

export default function Operations() {
  const operations = useCollection<Operation>(OPERATIONS);
  const panel = usePanel();
  const actions = useSubmission();
  const subject = operations.items.find((operation) => operation.metadata.name === panel.subject);
  const ordered = [...operations.items].sort(
    (first, second) => Number(Boolean(first.status.outcome)) - Number(Boolean(second.status.outcome)),
  );

  return (
    <div className="page">
      <PageHeader title="Operations" />
      <p className="u-text--muted">
        Migrations, evacuations, recoveries, upgrades and installs run as operations: a list of steps that survive the loss
        of any core member and can be paused or cancelled between steps.
      </p>
      <CollectionNotice state={operations.state} noun="operations" />
      {actions.problem && (
        <Notification severity="negative" title="The cluster refused" onDismiss={actions.clear}>
          {actions.problem}
        </Notification>
      )}
      <MainTable
        responsive
        emptyStateMsg="No operations."
        headers={[
          { content: "Name" },
          { content: "Kind" },
          { content: "Target" },
          { content: "Progress" },
          { content: "Status" },
          { content: "Actions", className: "u-align--right" },
        ]}
        rows={ordered.map((operation) => {
          const steps = operation.specification.steps.length;
          const completed = operation.status.completed_steps ?? 0;
          const isFinished = Boolean(operation.status.outcome);
          return {
            key: operation.metadata.name,
            columns: [
              { content: <SubjectLink name={operation.metadata.name} /> },
              { content: sentence(operation.specification.kind) },
              { content: targetOf(operation) },
              {
                content: (
                  <>
                    <Meter used={completed} total={steps} label={`${completed} of ${steps} steps`} />
                    <div className="secondary-line">{currentStepOf(operation)}</div>
                  </>
                ),
              },
              { content: <Status {...operationPhase(operation)} /> },
              {
                content: (
                  <div className="row-actions">
                    {!isFinished &&
                      (operation.specification.is_paused ? (
                        <IconAction icon="play" label={`Resume ${operation.metadata.name}`} onClick={() => void actions.submit(() => change(operation, { is_paused: false }))} />
                      ) : (
                        <IconAction icon="pause" label={`Pause ${operation.metadata.name}`} onClick={() => void actions.submit(() => change(operation, { is_paused: true }))} />
                      ))}
                    {!isFinished && (
                      <IconAction icon="stop" label={`Cancel ${operation.metadata.name}`} onClick={() => void actions.submit(() => change(operation, { is_cancelled: true }))} />
                    )}
                    {isFinished && (
                      <IconAction icon="delete" label={`Delete ${operation.metadata.name}`} onClick={() => panel.open("delete", operation.metadata.name)} />
                    )}
                  </div>
                ),
                className: "u-align--right",
              },
            ],
          };
        })}
      />
      {panel.panel === "detail" && subject && <OperationDetail operation={subject} onClose={panel.close} />}
      {panel.panel === "delete" && panel.subject && (
        <DeleteConfirmation scope={OPERATIONS} name={panel.subject} noun="operation" onClose={panel.close} />
      )}
    </div>
  );
}
