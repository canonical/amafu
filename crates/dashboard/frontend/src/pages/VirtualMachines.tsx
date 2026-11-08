import { Button, CheckboxInput, Input, MainTable, Notification, RadioInput, Select, Textarea } from "@canonical/react-components";
import { useState } from "react";

import { create } from "../cluster/client";
import type { Network, SecurityGroup, Vm, VmSpecification, Volume } from "../cluster/kinds";
import { isContainer } from "../cluster/kinds";
import { useCollection } from "../cluster/live";
import { projectScope } from "../cluster/scopes";
import CollectionNotice from "../components/CollectionNotice";
import Command from "../components/Command";
import DeleteConfirmation from "../components/DeleteConfirmation";
import PageHeader from "../components/PageHeader";
import Panel, { SubmitFooter } from "../components/Panel";
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

const STATUS_FILTERS = ["any", "running", "stopped", "migrating", "pending", "failed"];

function bootDescription(vm: Vm): string {
  const boot = vm.specification.boot;
  return boot.type === "kernel" ? `kernel ${boot.bundle}` : boot.type === "disk" ? "boots from disk" : boot.image;
}

export function NetworkChoices({
  chosen,
  onChange,
  label = "Networks",
}: {
  chosen: string[];
  onChange: (chosen: string[]) => void;
  label?: string;
}) {
  const project = useProject();
  const networks = useCollection<Network>({ plural: "networks", project });
  return (
    <fieldset className="choice-list">
      <legend>{label}</legend>
      {networks.items.length === 0 && <p className="u-text--muted">No networks in this project yet.</p>}
      {networks.items.map((network) => (
        <CheckboxInput
          key={network.metadata.name}
          label={`${network.metadata.name} · ${network.specification.subnet}`}
          checked={chosen.includes(network.metadata.name)}
          onChange={() =>
            onChange(
              chosen.includes(network.metadata.name)
                ? chosen.filter((name) => name !== network.metadata.name)
                : [...chosen, network.metadata.name],
            )
          }
        />
      ))}
    </fieldset>
  );
}

export function SecurityGroupChoices({ chosen, onChange }: { chosen: string[]; onChange: (chosen: string[]) => void }) {
  const project = useProject();
  const groups = useCollection<SecurityGroup>({ plural: "security-groups", project });
  if (groups.items.length === 0) {
    return null;
  }
  return (
    <fieldset className="choice-list">
      <legend>Security groups</legend>
      {groups.items.map((group) => (
        <CheckboxInput
          key={group.metadata.name}
          label={group.metadata.name}
          checked={chosen.includes(group.metadata.name)}
          onChange={() =>
            onChange(
              chosen.includes(group.metadata.name)
                ? chosen.filter((name) => name !== group.metadata.name)
                : [...chosen, group.metadata.name],
            )
          }
        />
      ))}
    </fieldset>
  );
}

export const MEMORY_CHOICES = [512, 1024, 2048, 4096, 8192, 16384, 32768, 65536, 131072];

function CreateVmPanel({ onClose }: { onClose: () => void }) {
  const project = useProject();
  const volumes = useCollection<Volume>({ plural: "volumes", project });
  const [name, setName] = useState("");
  const [cpus, setCpus] = useState(1);
  const [memory, setMemory] = useState(2048);
  const [bootKind, setBootKind] = useState<"kernel" | "disk">("disk");
  const [bundle, setBundle] = useState("");
  const [chosenVolumes, setChosenVolumes] = useState<string[]>([]);
  const [networks, setNetworks] = useState<string[]>([]);
  const [securityGroups, setSecurityGroups] = useState<string[]>([]);
  const [antiAffinity, setAntiAffinity] = useState("");
  const [priority, setPriority] = useState(0);
  const [userData, setUserData] = useState("");
  const [isStopped, setStopped] = useState(false);
  const { isBusy, problem, submit } = useSubmission();
  const freeVolumes = volumes.items.filter((volume) => !volume.status.attached_to && !volume.metadata.is_deleting);

  const specification: VmSpecification = {
    cpu_count: cpus,
    memory_mebibytes: memory,
    boot: bootKind === "kernel" ? { type: "kernel", bundle } : { type: "disk" },
    volumes: chosenVolumes,
    network_interfaces: networks.map((network) => ({ network, security_groups: securityGroups })),
    power_state: isStopped ? "stopped" : "running",
    priority,
    ...(antiAffinity ? { anti_affinity_group: antiAffinity } : {}),
    ...(userData ? { user_data: userData } : {}),
  };

  return (
    <Panel
      title="Create virtual machine"
      onClose={onClose}
      problem={problem}
      onSubmit={() => void submit(() => create(vmScope(project), name, specification), onClose)}
      footer={<SubmitFooter label="Create" isBusy={isBusy} onCancel={onClose} />}
    >
      <Input type="text" id="vm-name" label="Name" required value={name} onChange={(event) => setName(event.target.value)} />
      <div className="field-pair">
        <Input id="vm-cpus" label="vCPU" type="number" min={1} value={cpus} onChange={(event) => setCpus(Number(event.target.value))} />
        <Select
          id="vm-memory"
          label="Memory"
          value={memory}
          onChange={(event) => setMemory(Number(event.target.value))}
          options={MEMORY_CHOICES.map((choice) => ({ value: choice, label: mebibytes(choice) }))}
        />
      </div>
      <fieldset className="choice-list">
        <legend>Boot</legend>
        <RadioInput label="From the first volume" checked={bootKind === "disk"} onChange={() => setBootKind("disk")} />
        <RadioInput label="A kernel bundle on the nodes" checked={bootKind === "kernel"} onChange={() => setBootKind("kernel")} />
      </fieldset>
      {bootKind === "kernel" && (
        <Input type="text" id="vm-bundle" label="Kernel bundle" required value={bundle} onChange={(event) => setBundle(event.target.value)} />
      )}
      <fieldset className="choice-list">
        <legend>Volumes</legend>
        {freeVolumes.length === 0 && <p className="u-text--muted">No free volumes; create one on the Volumes page.</p>}
        {freeVolumes.map((volume) => (
          <CheckboxInput
            key={volume.metadata.name}
            label={`${volume.metadata.name} · ${volume.specification.size_gibibytes} GiB`}
            checked={chosenVolumes.includes(volume.metadata.name)}
            onChange={() =>
              setChosenVolumes(
                chosenVolumes.includes(volume.metadata.name)
                  ? chosenVolumes.filter((chosen) => chosen !== volume.metadata.name)
                  : [...chosenVolumes, volume.metadata.name],
              )
            }
          />
        ))}
      </fieldset>
      <NetworkChoices chosen={networks} onChange={setNetworks} />
      <SecurityGroupChoices chosen={securityGroups} onChange={setSecurityGroups} />
      <div className="field-pair">
        <Input id="vm-priority" label="Priority" type="number" value={priority} onChange={(event) => setPriority(Number(event.target.value))} />
        <Input type="text" id="vm-anti-affinity" label="Anti-affinity group" value={antiAffinity} onChange={(event) => setAntiAffinity(event.target.value)} />
      </div>
      <Textarea id="vm-user-data" label="Cloud-init user data" rows={3} value={userData} onChange={(event) => setUserData(event.target.value)} />
      <CheckboxInput label="Create it stopped" checked={isStopped} onChange={() => setStopped(!isStopped)} />
      <Command
        words={[
          "amafu",
          "vm",
          "create",
          name || "NAME",
          "--cpus",
          String(cpus),
          "--memory",
          String(memory),
          ...(bootKind === "kernel" ? ["--kernel", bundle || "BUNDLE"] : ["--from-disk"]),
          ...chosenVolumes.flatMap((volume) => ["--volume", volume]),
          ...networks.flatMap((network) => ["--network", network]),
          ...securityGroups.flatMap((group) => ["--security-group", group]),
          priority !== 0 && "--priority",
          priority !== 0 && String(priority),
          antiAffinity && "--anti-affinity-group",
          antiAffinity,
          userData && "--user-data",
          userData && "user-data.yaml",
          isStopped && "--stopped",
        ]}
      />
    </Panel>
  );
}

export default function VirtualMachines() {
  const project = useProject();
  const vms = useVms();
  const nodes = useNodes();
  const floatingAddresses = useFloatingAddresses();
  const panel = usePanel();
  const actions = useSubmission();
  const [query, setQuery] = useState("");
  const [statusFilter, setStatusFilter] = useState("any");
  const machines = vms.items.filter((vm) => !isContainer(vm));
  const shown = machines.filter(
    (vm) =>
      matches(query, vm.metadata.name, vm.status.node, bootDescription(vm)) &&
      (statusFilter === "any" || phaseOf(vm) === statusFilter),
  );
  const subject = machines.find((vm) => vm.metadata.name === panel.subject);

  return (
    <div className="page">
      <PageHeader title="Virtual machines">
        <SearchHeader noun="virtual machines" value={query} onChange={setQuery} />
        <Button appearance="positive" className="u-no-margin--bottom" onClick={() => panel.open("create")}>
          Create VM
        </Button>
      </PageHeader>
      <div className="filter-line">
        <Select
          className="u-no-margin--bottom filter-select"
          aria-label="Status"
          value={statusFilter}
          onChange={(event) => setStatusFilter(event.target.value)}
          options={STATUS_FILTERS.map((filter) => ({ value: filter, label: `Status: ${filter}` }))}
        />
        <span className="u-text--muted">
          {shown.length} of {machines.length} virtual machines
        </span>
      </div>
      <CollectionNotice state={vms.state} noun="virtual machines" />
      {actions.problem && (
        <Notification severity="negative" title="The cluster refused" onDismiss={actions.clear}>
          {actions.problem}
        </Notification>
      )}
      <MainTable
        sortable
        responsive
        className="instance-table"
        emptyStateMsg="No virtual machines match."
        headers={[
          { content: "Name", sortKey: "name" },
          { content: "Status", sortKey: "status" },
          { content: "Node", sortKey: "node" },
          { content: "vCPU", sortKey: "cpus", className: "u-align--right" },
          { content: "Memory", sortKey: "memory" },
          { content: "IPv4" },
          { content: "Floating IP" },
          { content: "Actions", className: "u-align--right" },
        ]}
        rows={shown.map((vm) => ({
          key: vm.metadata.name,
          columns: [
            {
              content: (
                <>
                  <SubjectLink name={vm.metadata.name} />
                  <div className="secondary-line">{bootDescription(vm)}</div>
                </>
              ),
            },
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
            { content: <span className="u-text--monospace">{(floatingAddresses.get(vm.metadata.name) ?? []).join(", ") || "—"}</span> },
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
          sortData: {
            name: vm.metadata.name,
            status: phaseOf(vm),
            node: vm.status.node ?? "",
            cpus: vm.specification.cpu_count,
            memory: vm.specification.memory_mebibytes,
          },
        }))}
      />
      {panel.panel === "create" && <CreateVmPanel onClose={panel.close} />}
      {panel.panel === "detail" && subject && (
        <InstanceDetail vm={subject} floatingAddresses={floatingAddresses.get(subject.metadata.name) ?? []} onClose={panel.close} />
      )}
      {panel.panel === "migrate" && subject && <MigratePanel vm={subject} nodes={nodes.items} onClose={panel.close} />}
      {panel.panel === "delete" && subject && (
        <DeleteConfirmation
          scope={projectScope("vms", project)}
          name={subject.metadata.name}
          noun="virtual machine"
          consequence="The VM stops and its interfaces are released. Its volumes are kept."
          onClose={panel.close}
        />
      )}
    </div>
  );
}
