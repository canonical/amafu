import { sentence } from "../format";

export type Tone = "positive" | "progress" | "caution" | "negative" | "neutral";

const TONES: Record<string, Tone> = {
  running: "positive",
  ready: "positive",
  available: "positive",
  attached: "positive",
  bound: "positive",
  allocated: "positive",
  succeeded: "positive",
  pending: "progress",
  scheduled: "progress",
  starting: "progress",
  stopping: "progress",
  migrating: "progress",
  "in-progress": "progress",
  paused: "caution",
  suspect: "caution",
  "not-ready": "caution",
  cordoned: "caution",
  unknown: "neutral",
  stopped: "neutral",
  cancelled: "neutral",
  unattached: "neutral",
  failed: "negative",
  lost: "negative",
  unreachable: "negative",
};

export function toneOf(phase: string | undefined): Tone {
  return TONES[phase ?? "unknown"] ?? "neutral";
}

interface StatusProps {
  phase?: string;
  label?: string;
  tone?: Tone;
  detail?: string;
}

export default function Status({ phase, label, tone, detail }: StatusProps) {
  return (
    <div>
      <span className={`status-dot status-dot--${tone ?? toneOf(phase)}`}>{label ?? sentence(phase ?? "unknown")}</span>
      {detail && <div className="u-text--muted u-no-margin--bottom secondary-line">{detail}</div>}
    </div>
  );
}
