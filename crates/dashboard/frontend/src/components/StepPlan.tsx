import type { PlanStatus } from "../cluster/environments";
import { sinceNow } from "../format";
import Meter from "./Meter";

export interface Plan<Step> {
  specification: { steps: Step[]; is_paused?: boolean };
  status: PlanStatus;
}

export function planPhase<Step>(plan: Plan<Step>): { phase: string; label?: string; detail?: string } {
  const outcome = plan.status.outcome;
  if (outcome) {
    return { phase: outcome.outcome, detail: outcome.outcome === "failed" ? outcome.reason : undefined };
  }
  if (plan.status.rollback) {
    return { phase: "stopping", label: "Rolling back", detail: `${plan.status.rollback.remaining_steps} steps to undo` };
  }
  if (plan.specification.is_paused) {
    return { phase: "paused" };
  }
  return { phase: "in-progress", label: "In progress", detail: plan.status.current_step?.last_error };
}

export function startedAt<Step>(plan: Plan<Step>): number {
  const moment = plan.status.finished_steps?.[0]?.started_at ?? plan.status.current_step?.started_at;
  if (moment) {
    return Date.parse(moment);
  }
  return plan.status.outcome ? Number.NEGATIVE_INFINITY : Number.POSITIVE_INFINITY;
}

export function sinceCreation<Change extends Plan<unknown> & { specification: { request: { kind: string } } }>(
  changes: Change[],
): Change[] {
  const newestFirst = [...changes].sort((first, second) => startedAt(second) - startedAt(first));
  const creation = newestFirst.findIndex((change) => change.specification.request.kind === "create");
  return creation < 0 ? newestFirst : newestFirst.slice(0, creation + 1);
}

export function StartedAt<Step>({ plan }: { plan: Plan<Step> }) {
  const moment = startedAt(plan);
  return Number.isFinite(moment) ? (
    <time dateTime={new Date(moment).toISOString()} title={new Date(moment).toLocaleString()}>
      {sinceNow(new Date(moment).toISOString())}
    </time>
  ) : (
    <span className="u-text--muted">{plan.status.outcome ? "—" : "Waiting"}</span>
  );
}

export function currentStep<Step>(plan: Plan<Step>, describe: (step: Step) => string): string {
  const step = plan.specification.steps[plan.status.completed_steps ?? 0];
  return plan.status.outcome || !step ? "" : describe(step);
}

export function PlanProgress<Step>({ plan, describe }: { plan: Plan<Step>; describe: (step: Step) => string }) {
  const steps = plan.specification.steps.length;
  const completed = plan.status.completed_steps ?? 0;
  return (
    <>
      <Meter used={completed} total={steps} label={`${completed} of ${steps} steps`} />
      <div className="secondary-line">{currentStep(plan, describe)}</div>
    </>
  );
}

export default function StepPlan<Step>({ plan, describe }: { plan: Plan<Step>; describe: (step: Step) => string }) {
  const completed = plan.status.completed_steps ?? 0;
  return (
    <ol className="step-list">
      {plan.specification.steps.map((step, index) => {
        const state = index < completed ? "done" : index === completed && !plan.status.outcome ? "current" : "waiting";
        return (
          <li key={index} className={`step-list__step step-list__step--${state}`}>
            {describe(step)}
            {state === "current" && plan.status.current_step && (
              <span className="secondary-line">
                {" "}
                started {sinceNow(plan.status.current_step.started_at)}, attempt {plan.status.current_step.attempts}
              </span>
            )}
          </li>
        );
      })}
    </ol>
  );
}
