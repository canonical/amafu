import { useSyncExternalStore } from "react";

import { announceSessionEnded } from "./client";
import type { Metadata } from "./kinds";

export type CollectionState = "loading" | "live" | "reconnecting" | "forbidden" | "failed";

export interface Collection<Item> {
  items: Item[];
  state: CollectionState;
}

type Frame =
  | { type: "snapshot"; items: { metadata: Metadata }[] }
  | { type: "changed"; object: { metadata: Metadata } }
  | { type: "removed"; project?: string; name: string };

type Message = { collection: string; frame: Frame } | { collection: string; refused: number };

const RETRY_MILLISECONDS = 2000;
const RESUBSCRIBE_MILLISECONDS = 30;
const EVERY_PROJECT = "*";

function keyOf(project: string | undefined, name: string): string {
  return `${project ?? ""}/${name}`;
}

function byName(first: { metadata: Metadata }, second: { metadata: Metadata }): number {
  return (
    (first.metadata.project ?? "").localeCompare(second.metadata.project ?? "") ||
    first.metadata.name.localeCompare(second.metadata.name, undefined, { numeric: true })
  );
}

class LiveCollection {
  readonly subscribe: (listener: () => void) => () => void;
  private readonly objects = new Map<string, { metadata: Metadata }>();
  readonly listeners = new Set<() => void>();
  private current: Collection<unknown> = { items: [], state: "loading" };

  constructor(subscribe: (listener: () => void) => () => void) {
    this.subscribe = subscribe;
  }

  read = (): Collection<unknown> => this.current;

  publish(state: CollectionState): void {
    this.current = { items: [...this.objects.values()].sort(byName), state };
    this.listeners.forEach((listener) => listener());
  }

  apply(frame: Frame): void {
    if (frame.type === "snapshot") {
      this.objects.clear();
      frame.items.forEach((item) => this.objects.set(keyOf(item.metadata.project, item.metadata.name), item));
    } else if (frame.type === "changed") {
      this.objects.set(keyOf(frame.object.metadata.project, frame.object.metadata.name), frame.object);
    } else {
      this.objects.delete(keyOf(frame.project, frame.name));
    }
    this.publish("live");
  }

  refuse(status: number): void {
    this.objects.clear();
    this.publish(status === 403 ? "forbidden" : "failed");
  }

  interrupt(): void {
    if (this.current.state === "live") {
      this.publish("reconnecting");
    }
  }
}

class LiveConnection {
  private readonly collections = new Map<string, LiveCollection>();
  private source: EventSource | null = null;
  private subscribed = "";
  private pendingChange: number | undefined;

  collection(key: string): LiveCollection {
    let collection = this.collections.get(key);
    if (!collection) {
      collection = new LiveCollection((listener) => this.subscribe(key, listener));
      this.collections.set(key, collection);
    }
    return collection;
  }

  subscribe(key: string, listener: () => void): () => void {
    const collection = this.collection(key);
    collection.listeners.add(listener);
    this.scheduleChange();
    return () => {
      collection.listeners.delete(listener);
      this.scheduleChange();
    };
  }

  forget(): void {
    window.clearTimeout(this.pendingChange);
    this.source?.close();
    this.source = null;
    this.subscribed = "";
    this.collections.clear();
  }

  private wanted(): string {
    return [...this.collections.entries()]
      .filter(([, collection]) => collection.listeners.size > 0)
      .map(([key]) => key)
      .sort()
      .join(",");
  }

  private scheduleChange(delay = RESUBSCRIBE_MILLISECONDS): void {
    window.clearTimeout(this.pendingChange);
    this.pendingChange = window.setTimeout(() => this.reconcile(), delay);
  }

  private reconcile(): void {
    const wanted = this.wanted();
    if (wanted === this.subscribed && this.source) {
      return;
    }
    this.source?.close();
    this.source = null;
    this.subscribed = wanted;
    if (wanted) {
      this.open(wanted);
    }
  }

  private open(keys: string): void {
    const source = new EventSource(`/events?collections=${encodeURIComponent(keys)}`);
    this.source = source;
    source.onmessage = (event) => {
      const message = JSON.parse(event.data) as Message;
      const collection = this.collections.get(message.collection);
      if (!collection) {
        return;
      }
      if ("refused" in message) {
        if (message.refused === 401) {
          announceSessionEnded();
        }
        collection.refuse(message.refused);
      } else {
        collection.apply(message.frame);
      }
    };
    source.onerror = () => {
      this.collections.forEach((collection) => collection.interrupt());
      if (source.readyState === EventSource.CLOSED) {
        void this.diagnose(source);
      }
    };
  }

  private async diagnose(source: EventSource): Promise<void> {
    const response = await fetch("/session", { credentials: "same-origin" }).catch(() => null);
    if (response?.status === 401) {
      announceSessionEnded();
      return;
    }
    if (this.source === source) {
      this.source = null;
      this.scheduleChange(RETRY_MILLISECONDS);
    }
  }
}

const connection = new LiveConnection();

export interface CollectionRequest {
  plural: string;
  project?: string | null;
  everyProject?: boolean;
}

function keyFor({ plural, project, everyProject }: CollectionRequest): string {
  if (everyProject) {
    return `${plural}@${EVERY_PROJECT}`;
  }
  return project ? `${plural}@${project}` : plural;
}

export function useCollection<Item>(request: CollectionRequest): Collection<Item> {
  const collection = connection.collection(keyFor(request));
  return useSyncExternalStore(collection.subscribe, collection.read) as Collection<Item>;
}

export function forgetCollections(): void {
  connection.forget();
}
