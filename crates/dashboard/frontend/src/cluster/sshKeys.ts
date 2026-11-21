import { call, projectQuery } from "./client";
import type { KeyLookup, KeyOrigin, KeySource, SshKey, SshKeyReference } from "./kinds";

const KEY_TYPE = "ssh-ed25519";
const MAGIC = "openssh-key-v1\0";
const LINE_WIDTH = 70;
const SEED_BYTES = 32;
const CHECK_BYTES = 4;
const BLOCK_SIZE = 8;
const GROUP_PREFIX = "group:";

export interface ParsedKey {
  algorithm: string;
  blob: string;
  comment?: string;
}

export function parsePublicKey(text: string): ParsedKey | undefined {
  const [algorithm, blob, ...comment] = text.trim().split(/\s+/);
  if (!algorithm || !blob) {
    return undefined;
  }
  return { algorithm, blob, ...(comment.length > 0 ? { comment: comment.join(" ") } : {}) };
}

export function publicKeyProblem(text: string): string | undefined {
  if (!text.trim()) {
    return undefined;
  }
  if (text.includes("PRIVATE KEY")) {
    return "This is a private key. Paste the public key (the .pub file) and keep the private key to yourself.";
  }
  if (text.trim().split("\n").length > 1) {
    return "Paste one key per line.";
  }
  return parsePublicKey(text) ? undefined : "This does not look like an OpenSSH public key.";
}

export function sameKey(left: string, right: string): boolean {
  return parsePublicKey(left)?.blob === parsePublicKey(right)?.blob;
}

export function describeOrigin(origin: KeyOrigin | undefined): string {
  if (!origin || origin.source === "pasted") {
    return "Pasted";
  }
  if (origin.source === "generated") {
    return "Generated";
  }
  return `${origin.source === "github" ? "GitHub" : "Launchpad"} · ${origin.account}`;
}

export async function fingerprint(publicKey: string): Promise<string> {
  const parsed = parsePublicKey(publicKey);
  if (!parsed) {
    return "";
  }
  const digest = await crypto.subtle.digest("SHA-256", base64Bytes(parsed.blob));
  return `SHA256:${bytesBase64(new Uint8Array(digest)).replace(/=+$/, "")}`;
}

export function lookUpKeys(project: string | null, source: KeySource, account: string): Promise<KeyLookup> {
  return call<KeyLookup>("GET", `/v1/ssh-key-sources/${source}/${encodeURIComponent(account)}${projectQuery(project)}`);
}

export function importName(source: KeySource, account: string, taken: string[]): string {
  const stem = `${source}-${account.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "")}`.slice(0, 56).replace(/-+$/, "");
  let index = 1;
  while (taken.includes(`${stem}-${index}`)) {
    index += 1;
  }
  return `${stem}-${index}`;
}

export function newKeys(lookup: KeyLookup, existing: SshKey[]): KeyLookup["keys"] {
  return lookup.keys.filter((discovered) => !existing.some((key) => sameKey(key.specification.public_key, discovered.public_key)));
}

export function referenceValue(reference: SshKeyReference): string {
  return reference.kind === "group" ? `${GROUP_PREFIX}${reference.name}` : reference.name;
}

export function referenceOf(value: string): SshKeyReference {
  return value.startsWith(GROUP_PREFIX) ? { kind: "group", name: value.slice(GROUP_PREFIX.length) } : { kind: "key", name: value };
}

export function grantWords(references: SshKeyReference[]): string[] {
  return references.flatMap((reference) => [reference.kind === "group" ? "--ssh-key-group" : "--ssh-key", reference.name]);
}

export interface GeneratedKeyPair {
  publicKey: string;
  privateKey: string;
}

export function canGenerate(): boolean {
  return typeof crypto !== "undefined" && Boolean(crypto.subtle);
}

export async function generateKeyPair(comment: string): Promise<GeneratedKeyPair> {
  const pair = (await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"])) as CryptoKeyPair;
  const publicBytes = new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey));
  const pkcs8 = new Uint8Array(await crypto.subtle.exportKey("pkcs8", pair.privateKey));
  const seed = pkcs8.slice(pkcs8.length - SEED_BYTES);
  const check = crypto.getRandomValues(new Uint8Array(CHECK_BYTES));
  return openSshKeyPair(seed, publicBytes, check, comment);
}

export function openSshKeyPair(seed: Uint8Array, publicBytes: Uint8Array, check: Uint8Array, comment: string): GeneratedKeyPair {
  const text = new TextEncoder();
  const blob = concat([sshString(text.encode(KEY_TYPE)), sshString(publicBytes)]);
  const unpadded = concat([
    check,
    check,
    sshString(text.encode(KEY_TYPE)),
    sshString(publicBytes),
    sshString(concat([seed, publicBytes])),
    sshString(text.encode(comment)),
  ]);
  const paddingLength = (BLOCK_SIZE - (unpadded.length % BLOCK_SIZE)) % BLOCK_SIZE;
  const padding = Uint8Array.from({ length: paddingLength }, (_, index) => index + 1);
  const document = concat([
    text.encode(MAGIC),
    sshString(text.encode("none")),
    sshString(text.encode("none")),
    sshString(new Uint8Array()),
    uint32(1),
    sshString(blob),
    sshString(concat([unpadded, padding])),
  ]);
  const body = bytesBase64(document).match(new RegExp(`.{1,${LINE_WIDTH}}`, "g")) ?? [];
  return {
    publicKey: `${KEY_TYPE} ${bytesBase64(blob)} ${comment}`,
    privateKey: `-----BEGIN OPENSSH PRIVATE KEY-----\n${body.join("\n")}\n-----END OPENSSH PRIVATE KEY-----\n`,
  };
}

export function download(filename: string, contents: string): void {
  const url = URL.createObjectURL(new Blob([contents], { type: "application/octet-stream" }));
  const link = document.createElement("a");
  link.href = url;
  link.download = filename;
  link.click();
  URL.revokeObjectURL(url);
}

function uint32(value: number): Uint8Array {
  const bytes = new Uint8Array(4);
  new DataView(bytes.buffer).setUint32(0, value);
  return bytes;
}

function sshString(value: Uint8Array): Uint8Array {
  return concat([uint32(value.length), value]);
}

function concat(parts: Uint8Array[]): Uint8Array {
  const joined = new Uint8Array(parts.reduce((total, part) => total + part.length, 0));
  let offset = 0;
  for (const part of parts) {
    joined.set(part, offset);
    offset += part.length;
  }
  return joined;
}

function bytesBase64(bytes: Uint8Array): string {
  return btoa(Array.from(bytes, (byte) => String.fromCharCode(byte)).join(""));
}

function base64Bytes(text: string): Uint8Array<ArrayBuffer> {
  return Uint8Array.from(atob(text), (character) => character.charCodeAt(0));
}
