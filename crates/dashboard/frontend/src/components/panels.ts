import { useCallback } from "react";
import { useSearchParams } from "react-router";

export interface PanelState {
  panel: string | null;
  subject: string | null;
  open: (panel: string, subject?: string) => void;
  close: () => void;
}

export function usePanel(): PanelState {
  const [parameters, setParameters] = useSearchParams();
  const open = useCallback(
    (panel: string, subject?: string) => {
      const next = new URLSearchParams(parameters);
      next.set("panel", panel);
      if (subject) {
        next.set("subject", subject);
      } else {
        next.delete("subject");
      }
      setParameters(next);
    },
    [parameters, setParameters],
  );
  const close = useCallback(() => {
    const next = new URLSearchParams(parameters);
    next.delete("panel");
    next.delete("subject");
    setParameters(next);
  }, [parameters, setParameters]);
  return { panel: parameters.get("panel"), subject: parameters.get("subject"), open, close };
}
