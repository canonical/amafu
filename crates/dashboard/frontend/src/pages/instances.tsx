import { Button, Notification, Select } from "@canonical/react-components";
import { useMemo, useState } from "react";

import { update } from "../cluster/client";
import type { FloatingIp, Node, PowerState, Vm, VmReference } from "../cluster/kinds";
import { useCollection } from "../cluster/live";
import { startMigration } from "../cluster/operations";
import { projectScope } from "../cluster/scopes";
import Command from "../components/Command";
import IconAction from "../components/IconAction";
import Panel from "../components/Panel";
import Status from "../components/Status";
import { useSubmission } from "../components/submission";
import { mebibytes, sentence } from "../format";
import { useProject } from "../session";

export function useVms(): ReturnType<typeof useCollection<Vm>> {
  const project = useProject();
  return useCollection<Vm>({ plural: "vms", project });
}

export function useNodes(): ReturnType<typeof useCollection<Node>> {
  return useCollection<Node>({ plural: "nodes" });
}

export function useFloatingAddresses(): Map<string, string[]> {
  const project = useProject();
  const floatingIps = useCollection<FloatingIp>({ plural: "floating-ips", project });
  return useMemo(() => {
    const addresses = new Map<string, string[]>();
    for (const floatingIp of floatingIps.items) {
      const vm = floatingIp.specification.target?.vm;
      const address = floatingIp.status.address ?? floatingIp.specification.address;
      if (vm && address) {
        addresses.set(vm, [...(addresses.get(vm) ?? []), address]);
      }
    }
    return addresses;
  }, [floatingIps.items]);
}

export function zoneOf(nodes: Node[], name: string | undefined): string | undefined {
  return nodes.find((node) => node.metadata.name === name)?.specification.zone;
}

export function referenceOf(vm: Vm, project: string | null): VmReference {
  return { project: vm.metadata.project ?? project ?? "default", name: vm.metadata.name };
}

export function vmScope(project: string | null) {
  return projectScope("vms", project);
}

export function setPower(vm: Vm, project: string | null, powerState: PowerState): Promise<unknown> {
  return update(vmScope(project), vm.metadata.name, { ...vm.specification, power_state: powerState }, vm.metadata.generation);
}

export function isRunning(vm: Vm): boolean {
  return (vm.specification.power_state ?? "running") === "running";
}

export function phaseOf(vm: Vm): string {
  if (vm.metadata.is_deleting) {
    return "stopping";
  }
  if (vm.status.migration && !["cleaned", "failed"].includes(vm.status.migration.phase)) {
    return "migrating";
  }
  return vm.status.phase ?? "pending";
}

export function InstanceStatus({ vm }: { vm: Vm }) {
  const phase = phaseOf(vm);
  const migration = vm.status.migration;
  if (phase === "migrating" && migration) {
    return <Status phase="migrating" detail={`${migration.source} → ${migration.destination}, ${sentence(migration.phase).toLowerCase()}`} />;
  }
  if (vm.metadata.is_deleting) {
    return <Status phase="stopping" label="Deleting" />;
  }
  return <Status phase={phase} detail={vm.status.phase === "failed" ? vm.status.message : undefined} />;
}

interface InstanceActionsProps {
  vm: Vm;
  onPower: (powerState: PowerState) => void;
  onMigrate: () => void;
  onDelete: () => void;
}

export function InstanceActions({ vm, onPower, onMigrate, onDelete }: InstanceActionsProps) {
  const running = isRunning(vm);
  return (
    <div className="row-actions">
      <IconAction
        icon={running ? "power-off" : "power-on"}
        label={running ? `Stop ${vm.metadata.name}` : `Start ${vm.metadata.name}`}
        onClick={() => onPower(running ? "stopped" : "running")}
      />
      <IconAction icon="change-version" label={`Migrate ${vm.metadata.name}`} onClick={onMigrate} isDisabled={!vm.status.node} />
      <IconAction icon="delete" label={`Delete ${vm.metadata.name}`} onClick={onDelete} />
    </div>
  );
}

interface MigratePanelProps {
  vm: Vm;
  nodes: Node[];
  onClose: () => void;
}

export function MigratePanel({ vm, nodes, onClose }: MigratePanelProps) {
  const project = useProject();
  const [destination, setDestination] = useState("");
  const { isBusy, problem, submit } = useSubmission();
  const candidates = nodes.filter(
    (node) =>
      node.metadata.name !== vm.status.node &&
      !node.specification.is_cordoned &&
      node.status.phase === "ready" &&
      (node.specification.roles ?? []).includes("compute"),
  );
  return (
    <Panel
      title={`Migrate ${vm.metadata.name}`}
      onClose={onClose}
      problem={problem}
      onSubmit={() => void submit(() => startMigration(referenceOf(vm, project), destination || null), onClose)}
      footer={
        <>
          <Button type="button" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" appearance="positive" disabled={isBusy}>
            Migrate
          </Button>
        </>
      }
    >
      <p>
        {vm.metadata.name} runs on <strong>{vm.status.node ?? "no node"}</strong>. It keeps running while its memory is
        copied; the pause at switch-over is short.
      </p>
      <Select
        id="destination"
        label="Destination"
        value={destination}
        onChange={(event) => setDestination(event.target.value)}
        options={[
          { value: "", label: "Let the scheduler choose" },
          ...candidates.map((node) => ({
            value: node.metadata.name,
            label: node.specification.zone ? `${node.metadata.name} · ${node.specification.zone}` : node.metadata.name,
          })),
        ]}
      />
      <Command words={["amafu", "vm", "migrate", vm.metadata.name, destination && "--to", destination]} />
    </Panel>
  );
}

interface DetailRow {
  label: string;
  value: React.ReactNode;
}

export function DetailList({ rows }: { rows: DetailRow[] }) {
  return (
    <table className="detail-list">
      <tbody>
        {rows.map(({ label, value }) => (
          <tr key={label}>
            <th>{label}</th>
            <td>{value ?? "—"}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

interface InstanceDetailProps {
  vm: Vm;
  floatingAddresses: string[];
  onClose: () => void;
}

export function InstanceDetail({ vm, floatingAddresses, onClose }: InstanceDetailProps) {
  const boot = vm.specification.boot;
  const interfaces = vm.status.interfaces ?? [];
  const isContainer = boot.type === "container";
  return (
    <Panel title={vm.metadata.name} onClose={onClose}>
      {vm.status.message && (
        <Notification severity={vm.status.phase === "failed" ? "negative" : "information"} title="Status">
          {vm.status.message}
        </Notification>
      )}
      <DetailList
        rows={[
          { label: "Status", value: <InstanceStatus vm={vm} /> },
          { label: "Node", value: vm.status.node },
          {
            label: isContainer ? "Image" : "Boots",
            value: boot.type === "container" ? boot.image : boot.type === "kernel" ? `kernel ${boot.bundle}` : "first volume",
          },
          { label: "vCPU", value: vm.specification.cpu_count },
          { label: "Memory", value: mebibytes(vm.specification.memory_mebibytes) },
          { label: "Volumes", value: (vm.specification.volumes ?? []).join(", ") || "—" },
          {
            label: "Interfaces",
            value:
              interfaces.length === 0
                ? (vm.specification.network_interfaces ?? []).map((nic) => nic.network).join(", ") || "—"
                : interfaces.map((nic) => `${nic.network} ${nic.address}`).join(", "),
          },
          { label: "Floating IPs", value: floatingAddresses.join(", ") || "—" },
          { label: "Priority", value: vm.specification.priority ?? 0 },
          { label: "Anti-affinity", value: vm.specification.anti_affinity_group },
        ]}
      />
      {vm.status.migration?.report && (
        <p className="u-text--muted">
          Last migration took {vm.status.migration.report.duration_milliseconds} ms with a{" "}
          {vm.status.migration.report.downtime_milliseconds} ms pause.
        </p>
      )}
      <Command words={["amafu", "vm", "show", vm.metadata.name]} />
    </Panel>
  );
}
