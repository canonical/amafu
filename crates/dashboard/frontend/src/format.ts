import type { Step } from "./cluster/kinds";

const MEBIBYTES_PER_GIBIBYTE = 1024;
const GIBIBYTES_PER_TEBIBYTE = 1024;
const BYTE_UNITS = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];

function rounded(value: number): string {
  return value >= 10 || Number.isInteger(value) ? Math.round(value).toString() : value.toFixed(1);
}

export function bytes(count: number): string {
  let value = count;
  let unit = 0;
  while (value >= 1024 && unit < BYTE_UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${rounded(value)} ${BYTE_UNITS[unit]}`;
}

export function mebibytes(count: number): string {
  return count >= MEBIBYTES_PER_GIBIBYTE ? gibibytes(count / MEBIBYTES_PER_GIBIBYTE) : `${count} MiB`;
}

export function gibibytes(count: number): string {
  return count >= GIBIBYTES_PER_TEBIBYTE ? `${rounded(count / GIBIBYTES_PER_TEBIBYTE)} TiB` : `${rounded(count)} GiB`;
}

export function sentence(identifier: string): string {
  const words = identifier.replaceAll("-", " ");
  return words.charAt(0).toUpperCase() + words.slice(1);
}

const SECONDS_PER_MINUTE = 60;
const SECONDS_PER_HOUR = 3600;
const SECONDS_PER_DAY = 86400;

export function sinceNow(moment: string | undefined, now: number = Date.now()): string {
  if (!moment) {
    return "—";
  }
  const seconds = Math.max(0, Math.round((now - Date.parse(moment)) / 1000));
  if (seconds < SECONDS_PER_MINUTE) {
    return `${seconds} s ago`;
  }
  if (seconds < SECONDS_PER_HOUR) {
    return `${Math.floor(seconds / SECONDS_PER_MINUTE)} min ago`;
  }
  if (seconds < SECONDS_PER_DAY) {
    return `${Math.floor(seconds / SECONDS_PER_HOUR)} h ago`;
  }
  return `${Math.floor(seconds / SECONDS_PER_DAY)} d ago`;
}

export function describeStep(step: Step): string {
  const subject = step.vm ? step.vm.name : step.node;
  const action = sentence(step.step);
  return subject ? `${action} ${subject}` : action;
}

export function shortDigest(digest: string): string {
  const [algorithm, value] = digest.includes(":") ? digest.split(":") : ["sha256", digest];
  return `${algorithm}:${value.slice(0, 12)}`;
}

export function countOf(count: number, singular: string, plural = `${singular}s`): string {
  return `${count} ${count === 1 ? singular : plural}`;
}
