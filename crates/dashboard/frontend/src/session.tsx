import { createContext, useCallback, useContext, useEffect, useMemo, useState } from "react";
import type { ReactNode } from "react";

import { call, onSessionEnded } from "./cluster/client";
import { forgetCollections } from "./cluster/live";

type SessionState = { phase: "checking" } | { phase: "signed-out" } | { phase: "signed-in"; project: string | null };

interface SessionActions {
  signIn: (token: string, project: string | null) => Promise<void>;
  signOut: () => Promise<void>;
}

const SessionContext = createContext<(SessionState & SessionActions) | null>(null);

export function SessionProvider({ children }: { children: ReactNode }) {
  const [state, setState] = useState<SessionState>({ phase: "checking" });

  useEffect(() => {
    let isCurrent = true;
    fetch("/session", { credentials: "same-origin" })
      .then(async (response) => {
        if (!isCurrent) {
          return;
        }
        if (response.ok) {
          const { project } = (await response.json()) as { project: string | null };
          setState({ phase: "signed-in", project });
        } else {
          setState({ phase: "signed-out" });
        }
      })
      .catch(() => isCurrent && setState({ phase: "signed-out" }));
    const stopListening = onSessionEnded(() => setState({ phase: "signed-out" }));
    return () => {
      isCurrent = false;
      stopListening();
    };
  }, []);

  const signIn = useCallback(async (token: string, project: string | null) => {
    await call("POST", "/session", { token, project });
    forgetCollections();
    setState({ phase: "signed-in", project });
  }, []);

  const signOut = useCallback(async () => {
    await fetch("/session", { method: "DELETE", credentials: "same-origin" });
    forgetCollections();
    setState({ phase: "signed-out" });
  }, []);

  const value = useMemo(() => ({ ...state, signIn, signOut }), [state, signIn, signOut]);
  return <SessionContext.Provider value={value}>{children}</SessionContext.Provider>;
}

export function useSession(): SessionState & SessionActions {
  const session = useContext(SessionContext);
  if (!session) {
    throw new Error("useSession needs a SessionProvider");
  }
  return session;
}

export function useProject(): string | null {
  const session = useSession();
  return session.phase === "signed-in" ? session.project : null;
}
