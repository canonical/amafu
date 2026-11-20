import { Input, Select, Switch, Textarea } from "@canonical/react-components";
import { useState } from "react";

import { chosenImage, ensureImage, imageChoices, useCatalog } from "../cluster/catalog";
import { create, createLabelled } from "../cluster/client";
import type { Image, Vm, VmSpecification } from "../cluster/kinds";
import { useCollection } from "../cluster/live";
import { projectScope } from "../cluster/scopes";
import ChipSelect from "../components/ChipSelect";
import Command from "../components/Command";
import SearchSelect from "../components/SearchSelect";
import Segments from "../components/Segments";
import Wizard from "../components/Wizard";
import { useSubmission } from "../components/submission";
import { mebibytes } from "../format";
import { useProject } from "../session";
import { NetworkChoices, SecurityGroupChoices, useBackendChoices, useFreeVolumeChoices } from "./attachments";
import { DetailList, vmScope } from "./instances";
import { nameProblem } from "./environmentForms";

export const MEMORY_CHOICES = [512, 1024, 2048, 4096, 8192, 16384, 32768, 65536, 131072];
const CPU_CHOICES = [1, 2, 4, 8, 16, 32];
const DEFAULT_DISK_GIBIBYTES = 20;
const MAXIMUM_DISK_GIBIBYTES = 16384;
const ROOT_OF = "amafu.io/root-of";
const GIBIBYTE = 1024 ** 3;

type BootFrom = "image" | "volume" | "kernel";

interface Source {
  bootFrom: BootFrom;
  image: string;
  diskSize: number;
  backend: string;
  isDiskKept: boolean;
  volume: string;
  bundle: string;
}

function SourceStep({ name, setName, taken, source, setSource }: {
  name: string;
  setName: (name: string) => void;
  taken: string[];
  source: Source;
  setSource: (source: Source) => void;
}) {
  const project = useProject();
  const images = useCollection<Image>({ plural: "images", project });
  const catalog = useCatalog();
  const backends = useBackendChoices();
  const volumes = useFreeVolumeChoices();
  const choices = imageChoices(images.items, catalog.entries, "virtual-machine");
  const picked = chosenImage(source.image, catalog.entries, "virtual-machine");
  const minimum = Math.max(1, Math.ceil((picked?.entry?.size_bytes ?? 0) / GIBIBYTE));
  return (
    <>
      <Input type="text" id="vm-name" label="Name" required value={name} error={nameProblem(name, taken)} onChange={(event) => setName(event.target.value)} />
      <Segments<BootFrom>
        label="Boot from"
        value={source.bootFrom}
        onChange={(bootFrom) => setSource({ ...source, bootFrom })}
        segments={[
          { value: "image", label: "A new disk from an image" },
          { value: "volume", label: "An existing volume" },
          { value: "kernel", label: "A kernel bundle" },
        ]}
      />
      {source.bootFrom === "image" && (
        <>
          <SearchSelect
            id="vm-image"
            label="Image"
            required
            value={source.image}
            choices={choices}
            onChange={(image) => setSource({ ...source, image })}
            placeholder={catalog.state === "loading" ? "Loading Canonical's images…" : "Search images"}
            help={catalog.state === "unavailable" ? "Canonical's image servers cannot be reached; only this project's images are offered." : "Images from Canonical are registered in the project when you create the VM."}
          />
          <div className="field-pair">
            <Input
              id="vm-disk-size"
              type="number"
              label="Disk size in GiB"
              min={minimum}
              max={MAXIMUM_DISK_GIBIBYTES}
              value={source.diskSize}
              error={source.diskSize < minimum ? `The image needs at least ${minimum} GiB` : undefined}
              onChange={(event) => setSource({ ...source, diskSize: Number(event.target.value) })}
            />
            <SearchSelect
              id="vm-backend"
              label="Storage backend"
              value={source.backend || backends[0]?.value || ""}
              choices={backends}
              onChange={(backend) => setSource({ ...source, backend })}
            />
          </div>
          <Switch
            label="Delete the disk when the VM is deleted"
            checked={!source.isDiskKept}
            onChange={() => setSource({ ...source, isDiskKept: !source.isDiskKept })}
          />
        </>
      )}
      {source.bootFrom === "volume" && (
        <SearchSelect
          id="vm-boot-volume"
          label="Boot volume"
          required
          value={source.volume}
          choices={volumes}
          onChange={(volume) => setSource({ ...source, volume })}
          placeholder="Search free volumes"
        />
      )}
      {source.bootFrom === "kernel" && (
        <Input type="text" id="vm-bundle" label="Kernel bundle" required value={source.bundle} onChange={(event) => setSource({ ...source, bundle: event.target.value })} />
      )}
    </>
  );
}

function isSourceReady(source: Source): boolean {
  switch (source.bootFrom) {
    case "image":
      return Boolean(source.image) && source.diskSize > 0;
    case "volume":
      return Boolean(source.volume);
    case "kernel":
      return Boolean(source.bundle);
  }
}

export default function CreateVmWizard({ taken, onClose }: { taken: string[]; onClose: () => void }) {
  const project = useProject();
  const catalog = useCatalog();
  const backends = useBackendChoices();
  const freeVolumes = useFreeVolumeChoices();
  const [name, setName] = useState("");
  const [source, setSource] = useState<Source>({
    bootFrom: "image",
    image: "",
    diskSize: DEFAULT_DISK_GIBIBYTES,
    backend: "",
    isDiskKept: false,
    volume: "",
    bundle: "",
  });
  const [cpus, setCpus] = useState(2);
  const [memory, setMemory] = useState(2048);
  const [extraVolumes, setExtraVolumes] = useState<string[]>([]);
  const [networks, setNetworks] = useState<string[]>([]);
  const [securityGroups, setSecurityGroups] = useState<string[]>([]);
  const [userData, setUserData] = useState("");
  const [priority, setPriority] = useState(0);
  const [antiAffinity, setAntiAffinity] = useState("");
  const [isStartedNow, setStartedNow] = useState(true);
  const { isBusy, problem, submit } = useSubmission();
  const backend = source.backend || backends[0]?.value || "";
  const picked = chosenImage(source.image, catalog.entries, "virtual-machine");
  const rootVolume = `${name}-root`;
  const bootVolumes = source.bootFrom === "image" ? [rootVolume] : source.bootFrom === "volume" ? [source.volume] : [];

  const specification: VmSpecification = {
    cpu_count: cpus,
    memory_mebibytes: memory,
    boot: source.bootFrom === "kernel" ? { type: "kernel", bundle: source.bundle } : { type: "disk" },
    volumes: [...bootVolumes, ...extraVolumes],
    network_interfaces: networks.map((network) => ({ network, security_groups: securityGroups })),
    power_state: isStartedNow ? "running" : "stopped",
    priority,
    ...(antiAffinity ? { anti_affinity_group: antiAffinity } : {}),
    ...(userData ? { user_data: userData } : {}),
  };

  const createVm = async () => {
    if (source.bootFrom === "image" && picked) {
      const image = await ensureImage(project, picked);
      const labels: Record<string, string> = source.isDiskKept ? {} : { [ROOT_OF]: name };
      await createLabelled(projectScope("volumes", project), rootVolume, labels, {
        size_gibibytes: source.diskSize,
        backend,
        source_image: image,
      });
    }
    return create(vmScope(project), name, specification);
  };

  const imageWord = picked?.entry?.id ?? picked?.name;
  const command = [
    "amafu", "vm", "create", name || "NAME",
    ...(source.bootFrom === "image"
      ? ["--image", imageWord || "IMAGE", "--disk-size", String(source.diskSize), "--storage-backend", backend || "BACKEND", source.isDiskKept && "--keep-disk"]
      : source.bootFrom === "volume"
        ? ["--from-disk", "--volume", source.volume || "VOLUME"]
        : ["--kernel", source.bundle || "BUNDLE"]),
    "--cpus", String(cpus), "--memory", String(memory),
    ...extraVolumes.flatMap((volume) => ["--volume", volume]),
    ...networks.flatMap((network) => ["--network", network]),
    ...securityGroups.flatMap((group) => ["--security-group", group]),
    priority !== 0 && "--priority", priority !== 0 && String(priority),
    antiAffinity && "--anti-affinity-group", antiAffinity,
    userData && "--user-data", userData && "user-data.yaml",
    !isStartedNow && "--stopped",
  ];
  const sourceSummary =
    source.bootFrom === "image" ? `${picked?.entry?.title ?? picked?.name ?? ""}, ${source.diskSize} GiB` : source.bootFrom === "volume" ? source.volume : source.bundle;

  return (
    <Wizard
      title="Create virtual machine"
      submitLabel="Create VM"
      isBusy={isBusy}
      problem={problem}
      onClose={onClose}
      onSubmit={() => void submit(createVm, onClose)}
      steps={[
        {
          title: "Image",
          summary: sourceSummary,
          isReady: Boolean(name) && !nameProblem(name, taken) && isSourceReady(source),
          content: <SourceStep name={name} setName={setName} taken={taken} source={source} setSource={setSource} />,
        },
        {
          title: "Resources",
          summary: `${cpus} vCPU, ${mebibytes(memory)}`,
          isReady: true,
          content: (
            <>
              <div className="field-pair">
                <Select id="vm-cpus" label="vCPU" value={cpus} onChange={(event) => setCpus(Number(event.target.value))} options={CPU_CHOICES.map((choice) => ({ value: choice, label: String(choice) }))} />
                <Select id="vm-memory" label="Memory" value={memory} onChange={(event) => setMemory(Number(event.target.value))} options={MEMORY_CHOICES.map((choice) => ({ value: choice, label: mebibytes(choice) }))} />
              </div>
              <ChipSelect
                id="vm-extra-volumes"
                label="More volumes"
                choices={freeVolumes.filter((volume) => !bootVolumes.includes(volume.value))}
                selected={extraVolumes}
                onChange={setExtraVolumes}
                placeholder="Search free volumes"
                emptyMessage="No free volumes"
                help="Attached after the boot disk, in this order."
              />
            </>
          ),
        },
        {
          title: "Network",
          summary: networks.length > 0 ? networks.join(", ") : "no network",
          isReady: true,
          content: (
            <>
              <NetworkChoices chosen={networks} onChange={setNetworks} />
              <SecurityGroupChoices chosen={securityGroups} onChange={setSecurityGroups} />
            </>
          ),
        },
        {
          title: "Review",
          isReady: true,
          content: (
            <div className="wizard__review">
              <div>
                <DetailList
                  rows={[
                    { label: "Name", value: name },
                    { label: "Boots", value: sourceSummary },
                    { label: "Size", value: `${cpus} vCPU, ${mebibytes(memory)}` },
                    { label: "Volumes", value: specification.volumes?.join(", ") || "—" },
                    { label: "Networks", value: networks.join(", ") || "—" },
                    { label: "Security groups", value: securityGroups.join(", ") || "—" },
                  ]}
                />
                <Switch label="Start it now" checked={isStartedNow} onChange={() => setStartedNow(!isStartedNow)} />
                <Command words={command} />
              </div>
              <div>
                <Textarea id="vm-user-data" label="Cloud-init user data" rows={5} value={userData} onChange={(event) => setUserData(event.target.value)} />
                <div className="field-pair">
                  <Input id="vm-priority" label="Priority" type="number" value={priority} onChange={(event) => setPriority(Number(event.target.value))} />
                  <Input type="text" id="vm-anti-affinity" label="Anti-affinity group" value={antiAffinity} onChange={(event) => setAntiAffinity(event.target.value)} />
                </div>
              </div>
            </div>
          ),
        },
      ]}
    />
  );
}

export function vmNames(vms: Vm[]): string[] {
  return vms.map((vm) => vm.metadata.name);
}
