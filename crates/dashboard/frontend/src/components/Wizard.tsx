import { Button, Step, Stepper } from "@canonical/react-components";
import type { ReactNode } from "react";
import { useState } from "react";

import Panel from "./Panel";

export interface WizardStep {
  title: string;
  summary?: string;
  isReady: boolean;
  content: ReactNode;
}

interface WizardProps {
  title: ReactNode;
  steps: WizardStep[];
  submitLabel: string;
  isBusy: boolean;
  problem?: string | null;
  onSubmit: () => void;
  onClose: () => void;
}

export default function Wizard({ title, steps, submitLabel, isBusy, problem, onSubmit, onClose }: WizardProps) {
  const [current, setCurrent] = useState(0);
  const step = steps[current];
  const isLast = current === steps.length - 1;
  const canReach = (index: number) => steps.slice(0, index).every((earlier) => earlier.isReady);
  const advance = () => {
    if (!step.isReady) {
      return;
    }
    if (isLast) {
      onSubmit();
    } else {
      setCurrent(current + 1);
    }
  };
  return (
    <Panel
      title={title}
      isWide
      onClose={onClose}
      problem={problem}
      onSubmit={advance}
      footer={
        <>
          <Button type="button" onClick={onClose}>
            Cancel
          </Button>
          {current > 0 && (
            <Button type="button" onClick={() => setCurrent(current - 1)}>
              Back
            </Button>
          )}
          <Button type="submit" appearance={isLast ? "positive" : "default"} disabled={!step.isReady || isBusy}>
            {isLast ? submitLabel : "Next"}
          </Button>
        </>
      }
    >
      <div className="wizard__steps">
        <Stepper
          variant="horizontal"
          steps={steps.map((candidate, index) => (
            <Step
              key={candidate.title}
              index={index + 1}
              title={candidate.title}
              label={index < current ? candidate.summary : undefined}
              hasProgressLine={index < steps.length - 1}
              enabled={index === current || canReach(index)}
              selected={index === current}
              iconName={index < current && candidate.isReady ? "success" : "number"}
              handleClick={() => {
                if (canReach(index)) {
                  setCurrent(index);
                }
              }}
            />
          ))}
        />
      </div>
      <div className="wizard__content">{step.content}</div>
    </Panel>
  );
}
