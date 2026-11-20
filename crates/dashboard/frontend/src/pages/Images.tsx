import { Button, Input, MainTable, Notification, Tabs } from "@canonical/react-components";
import { useState } from "react";

import { ensureImage, useCatalog } from "../cluster/catalog";
import { create } from "../cluster/client";
import type { Image, ImageSource } from "../cluster/kinds";
import { useCollection } from "../cluster/live";
import { projectScope } from "../cluster/scopes";
import CollectionNotice from "../components/CollectionNotice";
import Command from "../components/Command";
import DeleteConfirmation from "../components/DeleteConfirmation";
import IconAction from "../components/IconAction";
import PageHeader from "../components/PageHeader";
import Panel, { SubmitFooter } from "../components/Panel";
import Segments from "../components/Segments";
import SearchHeader, { matches } from "../components/SearchHeader";
import Status from "../components/Status";
import Tag from "../components/Tag";
import { usePanel } from "../components/panels";
import { useSubmission } from "../components/submission";
import { bytes, countOf } from "../format";
import { useProject } from "../session";

function sourceOf(image: Image): string {
  const source = image.specification.source;
  return source.type === "disk" ? source.url : source.reference;
}

function RegisterImagePanel({ onClose }: { onClose: () => void }) {
  const project = useProject();
  const [name, setName] = useState("");
  const [kind, setKind] = useState<ImageSource["type"]>("container");
  const [url, setUrl] = useState("");
  const [digest, setDigest] = useState("");
  const [reference, setReference] = useState("");
  const { isBusy, problem, submit } = useSubmission();
  const source: ImageSource = kind === "disk" ? { type: "disk", url, sha256: digest } : { type: "container", reference };

  return (
    <Panel
      title="Register your own image"
      onClose={onClose}
      problem={problem}
      onSubmit={() => void submit(() => create(projectScope("images", project), name, { source }), onClose)}
      footer={<SubmitFooter label="Register" isBusy={isBusy} onCancel={onClose} />}
    >
      <Input type="text" id="image-name" label="Name" required value={name} onChange={(event) => setName(event.target.value)} />
      <Segments<ImageSource["type"]>
        label="Type"
        value={kind}
        onChange={setKind}
        segments={[
          { value: "disk", label: "Disk image, raw or qcow2" },
          { value: "container", label: "Container image" },
        ]}
      />
      {kind === "container" ? (
        <Input type="text"
          id="image-reference"
          label="Reference"
          placeholder="ghcr.io/acme/api@sha256:…"
          required
          value={reference}
          onChange={(event) => setReference(event.target.value)}
        />
      ) : (
        <>
          <Input type="text" id="image-url" label="URL" required value={url} onChange={(event) => setUrl(event.target.value)} />
          <Input type="text" id="image-digest" label="SHA-256" required value={digest} onChange={(event) => setDigest(event.target.value)} />
        </>
      )}
      <Command
        words={[
          "amafu",
          "image",
          "create",
          name || "NAME",
          ...(kind === "container" ? ["--container", reference || "REFERENCE"] : ["--disk-url", url || "URL", "--sha256", digest || "HEX"]),
        ]}
      />
    </Panel>
  );
}

function publishedDate(published: string | undefined): string {
  const date = /^(\d{4})(\d{2})(\d{2})/.exec(published ?? "");
  return date ? `${date[1]}-${date[2]}-${date[3]}` : "—";
}

function CatalogTable({ query, images }: { query: string; images: Image[] }) {
  const project = useProject();
  const catalog = useCatalog();
  const actions = useSubmission();
  const owned = new Set(images.map((image) => image.metadata.name));
  const shown = catalog.entries.filter((entry) => matches(query, entry.title, entry.id, entry.distribution));
  if (catalog.state === "unavailable") {
    return (
      <Notification severity="caution" title="Canonical's image servers cannot be reached">
        The core members need HTTPS access to cloud-images.ubuntu.com, images.lxd.canonical.com and docker.io to offer
        Canonical's images.
      </Notification>
    );
  }
  return (
    <>
      {actions.problem && (
        <Notification severity="negative" title="The cluster refused" onDismiss={actions.clear}>
          {actions.problem}
        </Notification>
      )}
      <MainTable
        sortable
        responsive
        emptyStateMsg={catalog.state === "loading" ? "Loading Canonical's images…" : "No image matches."}
        headers={[
          { content: "Image", sortKey: "title" },
          { content: "For", sortKey: "kind" },
          { content: "Published", sortKey: "published" },
          { content: "Size", className: "u-align--right" },
          { content: "", className: "u-align--right" },
        ]}
        rows={shown.map((entry) => ({
          key: `${entry.kind}-${entry.id}`,
          columns: [
            {
              content: (
                <>
                  <span>{entry.title}</span> {entry.is_long_term && <Tag emphasis="information">LTS</Tag>}
                  <div className="secondary-line">{entry.id}</div>
                </>
              ),
            },
            { content: entry.kind === "container" ? "Containers" : "Virtual machines" },
            { content: publishedDate(entry.published) },
            { content: entry.size_bytes ? bytes(entry.size_bytes) : "—", className: "u-align--right" },
            {
              className: "u-align--right",
              content: owned.has(entry.name) ? (
                <span title={`Registered as ${entry.name}`}>
                  <Tag emphasis="information">In project</Tag>
                </span>
              ) : (
                <Button
                  dense
                  className="u-no-margin--bottom"
                  onClick={() => void actions.submit(() => ensureImage(project, { name: entry.name, entry }))}
                >
                  Add to project
                </Button>
              ),
            },
          ],
          sortData: { title: entry.title, kind: entry.kind, published: entry.published ?? "" },
        }))}
      />
    </>
  );
}

export default function Images() {
  const project = useProject();
  const images = useCollection<Image>({ plural: "images", project });
  const panel = usePanel();
  const [query, setQuery] = useState("");
  const [tab, setTab] = useState<"project" | "catalog">("project");
  const shown = images.items.filter((image) => matches(query, image.metadata.name, sourceOf(image)));

  return (
    <div className="page">
      <PageHeader title="Images">
        <SearchHeader noun="images" value={query} onChange={setQuery} />
        <Button appearance="positive" className="u-no-margin--bottom" onClick={() => panel.open("create")}>
          Register your own
        </Button>
      </PageHeader>
      <Tabs
        links={[
          { label: `In this project (${images.items.length})`, active: tab === "project", onClick: () => setTab("project") },
          { label: "From Canonical", active: tab === "catalog", onClick: () => setTab("catalog") },
        ]}
      />
      {tab === "catalog" ? (
        <CatalogTable query={query} images={images.items} />
      ) : (
        <>
          <CollectionNotice state={images.state} noun="images" />
          <MainTable
            sortable
            responsive
            emptyStateMsg="No images yet. Add one from Canonical's catalog, or register your own."
            headers={[
              { content: "Name", sortKey: "name" },
              { content: "Type", sortKey: "type" },
              { content: "Source" },
              { content: "Status", sortKey: "status" },
              { content: "Size", className: "u-align--right" },
              { content: "Cached on" },
              { content: "Actions", className: "u-align--right" },
            ]}
            rows={shown.map((image) => ({
              key: image.metadata.name,
              columns: [
                { content: image.metadata.name },
                { content: <Tag emphasis={image.specification.source.type === "container" ? "information" : "plain"}>{image.specification.source.type === "container" ? "OCI" : "Disk"}</Tag> },
                { content: <span className="u-text--monospace source-cell" title={sourceOf(image)}>{sourceOf(image)}</span> },
                { content: <Status phase={image.status.phase ?? "pending"} detail={image.status.phase === "failed" ? image.status.message : undefined} /> },
                { content: image.status.size_bytes ? bytes(image.status.size_bytes) : "—", className: "u-align--right" },
                { content: countOf((image.status.cached_on ?? []).length, "node") },
                {
                  content: <IconAction icon="delete" label={`Delete ${image.metadata.name}`} onClick={() => panel.open("delete", image.metadata.name)} />,
                  className: "u-align--right",
                },
              ],
              sortData: { name: image.metadata.name, type: image.specification.source.type, status: image.status.phase },
            }))}
          />
        </>
      )}
      {panel.panel === "create" && <RegisterImagePanel onClose={panel.close} />}
      {panel.panel === "delete" && panel.subject && (
        <DeleteConfirmation scope={projectScope("images", project)} name={panel.subject} noun="image" onClose={panel.close} />
      )}
    </div>
  );
}
