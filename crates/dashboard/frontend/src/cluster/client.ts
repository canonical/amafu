export interface Violation {
  field: string;
  problem: string;
}

export class ClusterError extends Error {
  readonly status: number;
  readonly violations: Violation[];

  constructor(status: number, message: string, violations: Violation[] = []) {
    super(message);
    this.status = status;
    this.violations = violations;
  }

  describe(): string {
    if (this.violations.length === 0) {
      return this.message;
    }
    return `${this.message}: ${this.violations.map((violation) => `${violation.field} ${violation.problem}`).join("; ")}`;
  }
}

type SessionEndedListener = () => void;

const sessionEndedListeners = new Set<SessionEndedListener>();

export function onSessionEnded(listener: SessionEndedListener): () => void {
  sessionEndedListeners.add(listener);
  return () => sessionEndedListeners.delete(listener);
}

export function announceSessionEnded(): void {
  sessionEndedListeners.forEach((listener) => listener());
}

async function answerOf(response: Response): Promise<unknown> {
  const text = await response.text();
  if (!text) {
    return null;
  }
  try {
    return JSON.parse(text);
  } catch {
    return { message: text };
  }
}

export async function call<Answer>(method: string, path: string, body?: unknown): Promise<Answer> {
  const response = await fetch(path, {
    method,
    credentials: "same-origin",
    headers: { "content-type": "application/json", "x-amafu-dashboard": "1" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const answer = (await answerOf(response)) as { message?: string; violations?: Violation[] } | null;
  if (response.status === 401) {
    announceSessionEnded();
  }
  if (!response.ok) {
    throw new ClusterError(
      response.status,
      answer?.message ?? `The cluster answered ${response.status}`,
      answer?.violations ?? [],
    );
  }
  return answer as Answer;
}

export function describeFailure(failure: unknown): string {
  if (failure instanceof ClusterError) {
    return failure.describe();
  }
  return failure instanceof Error ? failure.message : String(failure);
}

export function projectQuery(project: string | null, extra: Record<string, string> = {}): string {
  const parameters = new URLSearchParams(extra);
  if (project) {
    parameters.set("project", project);
  }
  const query = parameters.toString();
  return query ? `?${query}` : "";
}

export interface Scope {
  plural: string;
  project: string | null;
}

export function collectionPath({ plural, project }: Scope): string {
  return `/api/v1/${plural}${projectQuery(project)}`;
}

export function objectPath({ plural, project }: Scope, name: string): string {
  return `/api/v1/${plural}/${encodeURIComponent(name)}${projectQuery(project)}`;
}

export function create<Specification>(scope: Scope, name: string, specification: Specification): Promise<unknown> {
  return call("POST", collectionPath(scope), { metadata: { name }, specification });
}

export function update<Specification>(
  scope: Scope,
  name: string,
  specification: Specification,
  expectedGeneration: number,
): Promise<unknown> {
  return call("PUT", objectPath(scope, name), { specification, expected_generation: expectedGeneration });
}

export function remove(scope: Scope, name: string): Promise<unknown> {
  return call("DELETE", objectPath(scope, name));
}
