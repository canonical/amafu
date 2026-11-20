import { Button, Input, MainTable, Notification, Select, Textarea } from "@canonical/react-components";
import { useState } from "react";

import { create } from "../cluster/client";
import type { Image, Vm, VmSpecification } from "../cluster/kinds";
import { isContainer } from "../cluster/kinds";
import { chosenImage, ensureImage, imageChoices, useCatalog } from "../cluster/catalog";
import { useCollection } from "../cluster/live";
import CollectionNotice from "../components/CollectionNotice";
import Command from "../components/Command";
import DeleteConfirmation from "../components/DeleteConfirmation";
import PageHeader from "../components/PageHeader";
import SearchSelect from "../components/SearchSelect";
import Wizard from "../components/Wizard";
import SearchHeader, { matches } from "../components/SearchHeader";
import SubjectLink from "../components/SubjectLink";
import { usePanel } from "../components/panels";
import { useSubmission } from "../components/submission";
import { mebibytes } from "../format";
import { useProject } from "../session";
import {
  InstanceActions,
  InstanceDetail,
  InstanceStatus,
  MigratePanel,
  phaseOf,
  setPower,
  useFloatingAddresses,
  useNodes,
  useVms,
  vmScope,
  zoneOf,
} from "./instances";
import { NetworkChoices, SecurityGroupChoices } from "./attachments";
import { MEMORY_CHOICES } from "./CreateVmWizard";
import { nameProblem } from "./environmentForms";

const CONTAINER_CPU_COUNT = 1;
const CONTAINER_MEMORY_MEBIBYTES = 256;

function imageOf(vm: Vm): string {
  return vm.specification.boot.type === "container" ? vm.specification.boot.image : "";
}

function environmentOf(text: string): Record<string, string> {
  return Object.fromEntries(
    text
      .split("\n")
      .map((line) => line.trim())
      .filter((line) => line.includes("="))
      .map((line) => {
        const separator = line.indexOf("=");
        return [line.slice(0, separator), line.slice(separator + 1)];
      }),
  );
}

function CreateContainerWizard({ taken, onClose }: { taken: string[]; onClose: () => void }) {
  const project = useProject();
  const images = useCollection<Image>({ plural: "images", project });
  const catalog = useCatalog();
  const [name, setName] = useState("");
  const [image, setImage] = useState("");
  const [cpus, setCpus] = useState(CONTAINER_CPU_COUNT);
  const [memory, setMemory] = useState(CONTAINER_MEMORY_MEBIBYTES);
  const [networks, setNetworks] = useState<string[]>([]);
  const [securityGroups, setSecurityGroups] = useState<string[]>([]);
  const [environment, setEnvironment] = useState("");
  const [command, setCommand] = useState("");
  const { isBusy, problem, submit } = useSubmission();
  const picked = chosenImage(image, catalog.entries, "container");
  const commandWords = command.trim() ? command.trim().split(/\s+/) : [];
  const variables = environmentOf(environment);
  const specification = (imageName: string): VmSpecification => ({
    cpu_count: cpus,
    memory_mebibytes: memory,
    boot: { type: "container", image: imageName, command: commandWords, environment: variables },
    network_interfaces: networks.map((network) => ({ network, security_groups: securityGroups })),
    power_state: "running",
  });
  const run = async () => {
    if (!picked) {
      throw new Error("Choose an image");
    }
    return create(vmScope(project), name, specification(await ensureImage(project, picked)));
  };
  return (
    <Wizard
      title="Create container"
      submitLabel="Create container"
      isBusy={isBusy}
      problem={problem}
      onClose={onClose}
      onSubmit={() => void submit(run, onClose)}
      steps={[
        {
          title: "Image",
          summary: picked?.entry?.title ?? picked?.name,
          isReady: Boolean(name) && !nameProblem(name, taken) && Boolean(picked),
          content: (
            <>
              <Input type="text" id="container-name" label="Name" required value={name} error={nameProblem(name, taken)} onChange={(event) => setName(event.target.value)} />
              <SearchSelect
                id="container-image"
                label="Image"
                required
                value={image}
                choices={imageChoices(images.items, catalog.entries, "container")}
                onChange={setImage}
                placeholder={catalog.state === "loading" ? "Loading Canonical's images…" : "Search images"}
                help="Runs as an application container in its own small VM. Images from Canonical are pinned by digest."
              />
            </>
          ),
        },
        {
          title: "Resources",
          summary: `${cpus} vCPU, ${mebibytes(memory)}`,
          isReady: true,
          content: (
            <>
              <div className="field-pair">
                <Input id="container-cpus" label="vCPU" type="number" min={1} value={cpus} onChange={(event) => setCpus(Number(event.target.value))} />
                <Select
                  id="container-memory"
                  label="Memory"
                  value={memory}
                  onChange={(event) => setMemory(Number(event.target.value))}
                  options={[256, ...MEMORY_CHOICES].map((choice) => ({ value: choice, label: mebibytes(choice) }))}
                />
              </div>
              <NetworkChoices chosen={networks} onChange={setNetworks} />
              <SecurityGroupChoices chosen={securityGroups} onChange={setSecurityGroups} />
            </>
          ),
        },
        {
          title: "Command",
          isReady: true,
          content: (
            <div className="wizard__review">
              <div>
                <Input type="text" id="container-command" label="Command" help="Replaces the image's own command." value={command} onChange={(event) => setCommand(event.target.value)} />
                <Textarea id="container-environment" label="Environment" help="One NAME=VALUE per line." rows={4} value={environment} onChange={(event) => setEnvironment(event.target.value)} />
              </div>
              <Command
                words={[
                  "amafu", "container", "run", name || "NAME", "--image", picked?.entry?.id ?? picked?.name ?? "IMAGE",
                  cpus !== CONTAINER_CPU_COUNT && "--cpus", cpus !== CONTAINER_CPU_COUNT && String(cpus),
                  memory !== CONTAINER_MEMORY_MEBIBYTES && "--memory", memory !== CONTAINER_MEMORY_MEBIBYTES && String(memory),
                  ...networks.flatMap((network) => ["--network", network]),
                  ...securityGroups.flatMap((group) => ["--security-group", group]),
                  ...Object.entries(variables).flatMap(([variable, value]) => ["--env", `${variable}=${value}`]),
                  ...(commandWords.length > 0 ? ["--", ...commandWords] : []),
                ]}
              />
            </div>
          ),
        },
      ]}
    />
  );
}

export default function Containers() {
  const project = useProject();
  const vms = useVms();
  const nodes = useNodes();
  const floatingAddresses = useFloatingAddresses();
  const panel = usePanel();
  const actions = useSubmission();
  const [query, setQuery] = useState("");
  const containers = vms.items.filter(isContainer);
  const shown = containers.filter((vm) => matches(query, vm.metadata.name, imageOf(vm), vm.status.node));
  const subject = containers.find((vm) => vm.metadata.name === panel.subject);

  return (
    <div className="page">
      <PageHeader title="Containers">
        <SearchHeader noun="containers" value={query} onChange={setQuery} />
        <Button appearance="positive" className="u-no-margin--bottom" onClick={() => panel.open("create")}>
          Create container
        </Button>
      </PageHeader>
      <p className="u-text--muted">
        Each container runs in its own small VM, so it has the isolation, networking and live migration of any VM.{" "}
        {shown.length} of {containers.length} containers.
      </p>
      <CollectionNotice state={vms.state} noun="containers" />
      {actions.problem && (
        <Notification severity="negative" title="The cluster refused" onDismiss={actions.clear}>
          {actions.problem}
        </Notification>
      )}
      <MainTable
        sortable
        responsive
        emptyStateMsg="No containers match."
        headers={[
          { content: "Name", sortKey: "name" },
          { content: "Image", sortKey: "image" },
          { content: "Status", sortKey: "status" },
          { content: "Node", sortKey: "node" },
          { content: "vCPU", className: "u-align--right" },
          { content: "Memory" },
          { content: "IPv4" },
          { content: "Actions", className: "u-align--right" },
        ]}
        rows={shown.map((vm) => ({
          key: vm.metadata.name,
          columns: [
            { content: <SubjectLink name={vm.metadata.name} /> },
            { content: <span className="u-text--monospace">{imageOf(vm)}</span> },
            { content: <InstanceStatus vm={vm} /> },
            {
              content: (
                <>
                  {vm.status.node ?? "—"}
                  <div className="secondary-line">{zoneOf(nodes.items, vm.status.node) ?? ""}</div>
                </>
              ),
            },
            { content: vm.specification.cpu_count, className: "u-align--right" },
            { content: mebibytes(vm.specification.memory_mebibytes) },
            { content: <span className="u-text--monospace">{(vm.status.interfaces ?? []).map((nic) => nic.address).join(", ") || "—"}</span> },
            {
              content: (
                <InstanceActions
                  vm={vm}
                  onPower={(powerState) => void actions.submit(() => setPower(vm, project, powerState))}
                  onMigrate={() => panel.open("migrate", vm.metadata.name)}
                  onDelete={() => panel.open("delete", vm.metadata.name)}
                />
              ),
              className: "u-align--right",
            },
          ],
          sortData: { name: vm.metadata.name, image: imageOf(vm), status: phaseOf(vm), node: vm.status.node ?? "" },
        }))}
      />
      {panel.panel === "create" && <CreateContainerWizard taken={vms.items.map((vm) => vm.metadata.name)} onClose={panel.close} />}
      {panel.panel === "detail" && subject && (
        <InstanceDetail vm={subject} floatingAddresses={floatingAddresses.get(subject.metadata.name) ?? []} onClose={panel.close} />
      )}
      {panel.panel === "migrate" && subject && <MigratePanel vm={subject} nodes={nodes.items} onClose={panel.close} />}
      {panel.panel === "delete" && subject && (
        <DeleteConfirmation scope={vmScope(project)} name={subject.metadata.name} noun="container" onClose={panel.close} />
      )}
    </div>
  );
}
