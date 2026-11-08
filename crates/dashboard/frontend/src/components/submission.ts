import { useCallback, useState } from "react";

import { describeFailure } from "../cluster/client";

export interface Submission {
  isBusy: boolean;
  problem: string | null;
  submit: (action: () => Promise<unknown>, onDone?: () => void) => Promise<void>;
  clear: () => void;
}

export function useSubmission(): Submission {
  const [isBusy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);

  const submit = useCallback(async (action: () => Promise<unknown>, onDone?: () => void) => {
    setBusy(true);
    setProblem(null);
    try {
      await action();
      onDone?.();
    } catch (failure) {
      setProblem(describeFailure(failure));
    } finally {
      setBusy(false);
    }
  }, []);

  const clear = useCallback(() => setProblem(null), []);
  return { isBusy, problem, submit, clear };
}
