import { Button, Input, MainTable, Select } from "@canonical/react-components";
import { useState } from "react";

import { create } from "../cluster/client";
import type { Image, StorageBackend, Volume } from "../cluster/kinds";
import { useCollection } from "../cluster/live";
import { projectScope } from "../cluster/scopes";
import CollectionNotice from "../components/CollectionNotice";
import Command from "../components/Command";
import DeleteConfirmation from "../components/DeleteConfirmation";
import IconAction from "../components/IconAction";
import PageHeader from "../components/PageHeader";
import Panel, { SubmitFooter } from "../components/Panel";
import SearchHeader, { matches } from "../components/SearchHeader";
import Status from "../components/Status";
import { usePanel } from "../components/panels";
import { useSubmission } from "../components/submission";
import { gibibytes } from "../format";
import { useProject } from "../session";

function CreateVolumePanel({ onClose }: { onClose: () => void }) {
  const project = useProject();
  const backends = useCollection<StorageBackend>({ plural: "storage-backends" });
  const images = useCollection<Image>({ plural: "images", project });
  const diskImages = images.items.filter((image) => image.specification.source.type === "disk");
  const [name, setName] = useState("");
  const [size, setSize] = useState(20);
  const [backend, setBackend] = useState("");
  const [sourceImage, setSourceImage] = useState("");
  const { isBusy, problem, submit } = useSubmission();
  const chosenBackend = backend || backends.items[0]?.metadata.name || "";
  const specification = {
    size_gibibytes: size,
    backend: chosenBackend,
    ...(sourceImage ? { source_image: sourceImage } : {}),
  };

  return (
    <Panel
      title="Create volume"
      onClose={onClose}
      problem={problem}
      onSubmit={() => void submit(() => create(projectScope("volumes", project), name, specification), onClose)}
      footer={<SubmitFooter label="Create" isBusy={isBusy} onCancel={onClose} />}
    >
      <Input type="text" id="volume-name" label="Name" required value={name} onChange={(event) => setName(event.target.value)} />
      <Input id="volume-size" label="Size in GiB" type="number" min={1} value={size} onChange={(event) => setSize(Number(event.target.value))} />
      <Select
        id="volume-backend"
        label="Storage backend"
        value={chosenBackend}
        onChange={(event) => setBackend(event.target.value)}
        options={backends.items.map((candidate) => ({ value: candidate.metadata.name, label: candidate.metadata.name }))}
      />
      <Select
        id="volume-image"
        label="Clone from image"
        value={sourceImage}
        onChange={(event) => setSourceImage(event.target.value)}
        options={[{ value: "", label: "Empty" }, ...diskImages.map((image) => ({ value: image.metadata.name, label: image.metadata.name }))]}
      />
      <Command
        words={[
          "amafu",
          "volume",
          "create",
          name || "NAME",
          "--size",
          String(size),
          "--backend",
          chosenBackend || "BACKEND",
          sourceImage && "--from-image",
          sourceImage,
        ]}
      />
    </Panel>
  );
}

export default function Volumes() {
  const project = useProject();
  const volumes = useCollection<Volume>({ plural: "volumes", project });
  const panel = usePanel();
  const [query, setQuery] = useState("");
  const shown = volumes.items.filter((volume) =>
    matches(query, volume.metadata.name, volume.specification.backend, volume.status.attached_to),
  );

  return (
    <div className="page">
      <PageHeader title="Volumes">
        <SearchHeader noun="volumes" value={query} onChange={setQuery} />
        <Button appearance="positive" className="u-no-margin--bottom" onClick={() => panel.open("create")}>
          Create volume
        </Button>
      </PageHeader>
      <CollectionNotice state={volumes.state} noun="volumes" />
      <MainTable
        sortable
        responsive
        emptyStateMsg="No volumes yet."
        headers={[
          { content: "Name", sortKey: "name" },
          { content: "Size", sortKey: "size", className: "u-align--right" },
          { content: "Backend", sortKey: "backend" },
          { content: "Cloned from" },
          { content: "Status", sortKey: "status" },
          { content: "Attached to", sortKey: "attached" },
          { content: "Node" },
          { content: "Actions", className: "u-align--right" },
        ]}
        rows={shown.map((volume) => ({
          key: volume.metadata.name,
          columns: [
            { content: volume.metadata.name },
            { content: gibibytes(volume.specification.size_gibibytes), className: "u-align--right" },
            { content: volume.specification.backend },
            { content: volume.specification.source_image ?? "—" },
            { content: <Status phase={volume.metadata.is_deleting ? "stopping" : (volume.status.phase ?? "pending")} label={volume.metadata.is_deleting ? "Deleting" : undefined} detail={volume.status.phase === "failed" ? volume.status.message : undefined} /> },
            { content: volume.status.attached_to ?? "—" },
            { content: volume.status.provisioned?.node ?? "—" },
            {
              content: (
                <IconAction
                  icon="delete"
                  label={`Delete ${volume.metadata.name}`}
                  isDisabled={Boolean(volume.status.attached_to)}
                  onClick={() => panel.open("delete", volume.metadata.name)}
                />
              ),
              className: "u-align--right",
            },
          ],
          sortData: {
            name: volume.metadata.name,
            size: volume.specification.size_gibibytes,
            backend: volume.specification.backend,
            status: volume.status.phase,
            attached: volume.status.attached_to ?? "",
          },
        }))}
      />
      {panel.panel === "create" && <CreateVolumePanel onClose={panel.close} />}
      {panel.panel === "delete" && panel.subject && (
        <DeleteConfirmation
          scope={projectScope("volumes", project)}
          name={panel.subject}
          noun="volume"
          consequence="The volume and its data are removed from the storage backend."
          onClose={panel.close}
        />
      )}
    </div>
  );
}
