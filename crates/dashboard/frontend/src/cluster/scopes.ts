import type { Scope } from "./client";

export function projectScope(plural: string, project: string | null): Scope {
  return { plural, project };
}

export function clusterScope(plural: string): Scope {
  return { plural, project: null };
}
