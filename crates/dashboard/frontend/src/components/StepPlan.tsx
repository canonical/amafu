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
