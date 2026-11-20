import { useEffect, useState } from "react";

import { call, create } from "./client";
import type { Image, ImageSource } from "./kinds";
import { projectScope } from "./scopes";
import type { Choice } from "../components/SearchSelect";
import { bytes } from "../format";

export type CatalogKind = "virtual-machine" | "container";

export interface CatalogEntry {
  id: string;
  name: string;
  title: string;
  distribution: string;
  kind: CatalogKind;
  source: ImageSource;
  size_bytes?: number;
  published?: string;
  is_long_term?: boolean;
}

export interface Catalog {
  entries: CatalogEntry[];
  state: "loading" | "ready" | "unavailable";
}

let fetched: Promise<CatalogEntry[]> | null = null;

function catalogEntries(): Promise<CatalogEntry[]> {
  fetched ??= call<CatalogEntry[]>("GET", "/api/v1/image-catalog").catch((failure: unknown) => {
    fetched = null;
    throw failure;
  });
  return fetched;
}

export function useCatalog(): Catalog {
  const [catalog, setCatalog] = useState<Catalog>({ entries: [], state: "loading" });
  useEffect(() => {
    let isCurrent = true;
    catalogEntries()
      .then((entries) => isCurrent && setCatalog({ entries, state: "ready" }))
      .catch(() => isCurrent && setCatalog({ entries: [], state: "unavailable" }));
    return () => {
      isCurrent = false;
    };
  }, []);
  return catalog;
}

const PROJECT_IMAGE = "image:";
const CATALOG_IMAGE = "catalog:";

export function kindOf(image: Image): CatalogKind {
  return image.specification.source.type === "container" ? "container" : "virtual-machine";
}

function catalogDetail(entry: CatalogEntry): string {
  return [entry.is_long_term ? "LTS" : "", entry.size_bytes ? bytes(entry.size_bytes) : "", entry.published ? `published ${entry.published.slice(0, 4)}-${entry.published.slice(4, 6)}-${entry.published.slice(6, 8)}` : ""]
    .filter(Boolean)
    .join(" · ");
}

export function imageChoices(images: Image[], catalog: CatalogEntry[], kind: CatalogKind): Choice[] {
  const own = images.filter((image) => kindOf(image) === kind && !image.metadata.is_deleting);
  const ownNames = new Set(own.map((image) => image.metadata.name));
  return [
    ...own.map((image) => ({
      value: `${PROJECT_IMAGE}${image.metadata.name}`,
      label: image.metadata.name,
      detail: image.status.phase === "ready" ? "ready" : (image.status.phase ?? "pending"),
      group: "In this project",
    })),
    ...catalog
      .filter((entry) => entry.kind === kind && !ownNames.has(entry.name))
      .map((entry) => ({
        value: `${CATALOG_IMAGE}${entry.id}`,
        label: entry.title,
        detail: catalogDetail(entry),
        group: entry.distribution === "ubuntu" ? "Ubuntu from Canonical" : "Other distributions from Canonical",
      })),
  ];
}

export interface ChosenImage {
  name: string;
  entry?: CatalogEntry;
}

export function chosenImage(choice: string, catalog: CatalogEntry[], kind: CatalogKind): ChosenImage | null {
  if (choice.startsWith(PROJECT_IMAGE)) {
    return { name: choice.slice(PROJECT_IMAGE.length) };
  }
  const entry = catalog.find((candidate) => candidate.kind === kind && `${CATALOG_IMAGE}${candidate.id}` === choice);
  return entry ? { name: entry.name, entry } : null;
}

export async function ensureImage(project: string | null, image: ChosenImage): Promise<string> {
  if (image.entry) {
    await create(projectScope("images", project), image.name, { source: image.entry.source }).catch((failure: unknown) => {
      if (!(failure instanceof Error && "status" in failure && failure.status === 409)) {
        throw failure;
      }
    });
  }
  return image.name;
}
