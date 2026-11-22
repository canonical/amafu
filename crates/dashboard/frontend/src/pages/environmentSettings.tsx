import { Button, Input, Select } from "@canonical/react-components";
import { useState } from "react";

import { useCatalog } from "../cluster/catalog";
import { create, update } from "../cluster/client";
import type { EnvironmentSettings, ImageSource } from "../cluster/kinds";
import { useCollection } from "../cluster/live";
import { clusterScope } from "../cluster/scopes";
import CollectionNotice from "../components/CollectionNotice";
import Command from "../components/Command";
import Panel, { SubmitFooter } from "../components/Panel";
import { usePanel } from "../components/panels";
import { useSubmission } from "../components/submission";
import { DetailList } from "./instances";

const SETTINGS = clusterScope("environment-settings");
const SETTINGS_NAME = "cluster";

function sourceLabel(source: ImageSource | undefined, titles: Map<string, string>): string | undefined {
  if (!source) {
    return undefined;
  }
  const location = source.type === "disk" ? source.url : source.reference;
  return titles.get(location) ?? location;
}

function ChangePanel({ settings, onClose }: { settings?: EnvironmentSettings; onClose: () => void }) {
  const catalog = useCatalog();
  const ubuntu = catalog.entries.filter((entry) => entry.kind === "virtual-machine" && entry.distribution === "ubuntu");
  const current = settings?.specification.ubuntu_image;
  const currentId = ubuntu.find((entry) => JSON.stringify(entry.source) === JSON.stringify(current))?.id ?? "";
  const [ubuntuId, setUbuntuId] = useState(currentId);
  const [runner, setRunner] = useState(settings?.specification.juju_runner_image ?? "");
  const { isBusy, problem, submit } = useSubmission();
  const chosen = ubuntu.find((entry) => entry.id === ubuntuId);
  const specification = {
    ubuntu_image: chosen?.source ?? current,
    juju_runner_image: runner || undefined,
  };
  const save = () =>
    settings
      ? update(SETTINGS, SETTINGS_NAME, specification, settings.metadata.generation)
      : create(SETTINGS, SETTINGS_NAME, specification);
  return (
    <Panel
      title="Environment images"
      onClose={onClose}
      problem={problem}
      onSubmit={() => void submit(save, onClose)}
      footer={<SubmitFooter label="Save" isBusy={isBusy} onCancel={onClose} />}
    >
      <Select
        id="environment-ubuntu"
        label="Ubuntu image"
        help="Kubernetes nodes and Juju controller machines boot from it."
        value={ubuntuId}
        onChange={(event) => setUbuntuId(event.target.value)}
        options={[{ value: "", label: catalog.state === "loading" ? "Loading the catalog…" : "Choose an image" }, ...ubuntu.map((entry) => ({ value: entry.id, label: entry.title }))]}
      />
      <Input
        type="text"
        id="environment-runner"
        label="Juju runner image"
        help="Only Juju controllers on Kubernetes need it, as REGISTRY/REPOSITORY@sha256:DIGEST."
        value={runner}
        onChange={(event) => setRunner(event.target.value)}
      />
      <Command
        words={["amafu", "environment-settings", "set", chosen && "--ubuntu-image", chosen?.id, runner && "--juju-runner-image", runner || undefined]}
      />
    </Panel>
  );
}

export default function EnvironmentImages() {
  const settings = useCollection<EnvironmentSettings>(SETTINGS);
  const catalog = useCatalog();
  const panel = usePanel();
  const current = settings.items.find((item) => item.metadata.name === SETTINGS_NAME);
  const titles = new Map(catalog.entries.map((entry) => [entry.source.type === "disk" ? entry.source.url : entry.source.reference, entry.title]));
  return (
    <>
      <div className="section-header">
        <h2 className="p-heading--4 u-no-margin--bottom">Environment images</h2>
        <Button className="u-no-margin--bottom" onClick={() => panel.open("environment-images")}>
          Change
        </Button>
      </div>
      <CollectionNotice state={settings.state} noun="environment settings" />
      <DetailList
        rows={[
          { label: "Ubuntu image", value: sourceLabel(current?.specification.ubuntu_image, titles) ?? "Not set; Kubernetes clusters and Juju controllers cannot be created" },
          { label: "Juju runner image", value: current?.specification.juju_runner_image ?? "Not set; needed only for Juju controllers on Kubernetes" },
        ]}
      />
      {panel.panel === "environment-images" && <ChangePanel settings={current} onClose={panel.close} />}
    </>
  );
}
