import { Button, MainTable, Notification, Select } from "@canonical/react-components";
import { useState } from "react";

import type { GuestInventory, Vm, Volume } from "../cluster/kinds";
import { useCollection } from "../cluster/live";
import { isContainer } from "../cluster/kinds";
import { projectScope } from "../cluster/scopes";
import CollectionNotice from "../components/CollectionNotice";
import DeleteConfirmation from "../components/DeleteConfirmation";
import PageHeader from "../components/PageHeader";
import SearchHeader, { matches } from "../components/SearchHeader";
import GuestPills, { guestCount } from "../components/GuestPills";
import SubjectLink from "../components/SubjectLink";
import { usePanel } from "../components/panels";
import { useSubmission } from "../components/submission";
import CreateVmWizard, { vmNames } from "./CreateVmWizard";
import { countOf, mebibytes } from "../format";
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
  zoneOf,
} from "./instances";

const ROOT_OF = "amafu.io/root-of";
const STATUS_FILTERS = ["any", "running", "stopped", "migrating", "pending", "failed"];

function bootDescription(vm: Vm): string {
  const boot = vm.specification.boot;
  return boot.type === "kernel" ? `kernel ${boot.bundle}` : boot.type === "disk" ? "boots from disk" : boot.image;
}

function deletionConsequence(vm: Vm, volumes: Volume[]): string {
  const disks = volumes.filter((volume) => volume.metadata.labels?.[ROOT_OF] === vm.metadata.name).map((volume) => volume.metadata.name);
  return disks.length > 0
    ? `The VM stops and its interfaces are released. Its disk ${disks.join(", ")} is deleted with it; other volumes are kept.`
    : "The VM stops and its interfaces are released. Its volumes are kept.";
}

export default function VirtualMachines() {
  const project = useProject();
  const volumes = useCollection<Volume>({ plural: "volumes", project });
  const inventories = useCollection<GuestInventory>({ plural: "guest-inventories", project });
  const inventoryOf = new Map(inventories.items.map((inventory) => [inventory.metadata.name, inventory]));
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(new Set());
  const toggle = (name: string) =>
    setExpanded((current) => {
      const next = new Set(current);
      if (!next.delete(name)) {
        next.add(name);
      }
      return next;
    });
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
        expanding
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
        rows={shown.map((vm) => {
          const inventory = inventoryOf.get(vm.metadata.name);
          const isExpanded = Boolean(inventory) && expanded.has(vm.metadata.name);
          return {
          key: vm.metadata.name,
          expanded: isExpanded,
          expandedContent: isExpanded && inventory ? <GuestPills inventory={inventory} /> : undefined,
          columns: [
            {
              content: (
                <>
                  {inventory && (
                    <button
                      type="button"
                      className="guest-toggle"
                      aria-expanded={isExpanded}
                      aria-label={`${isExpanded ? "Hide" : "Show"} what ${vm.metadata.name} hosts`}
                      onClick={() => toggle(vm.metadata.name)}
                    >
                      <i className={isExpanded ? "p-icon--chevron-up" : "p-icon--chevron-down"} />
                    </button>
                  )}
                  <SubjectLink name={vm.metadata.name} />
                  <div className="secondary-line">
                    {bootDescription(vm)}
                    {guestCount(inventory) > 0 && ` · hosts ${countOf(guestCount(inventory), "guest")}`}
                  </div>
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
          };
        })}
      />
      {panel.panel === "create" && <CreateVmWizard taken={vmNames(vms.items)} onClose={panel.close} />}
      {panel.panel === "detail" && subject && (
        <InstanceDetail vm={subject} floatingAddresses={floatingAddresses.get(subject.metadata.name) ?? []} onClose={panel.close} />
      )}
      {panel.panel === "migrate" && subject && <MigratePanel vm={subject} nodes={nodes.items} onClose={panel.close} />}
      {panel.panel === "delete" && subject && (
        <DeleteConfirmation
          scope={projectScope("vms", project)}
          name={subject.metadata.name}
          noun="virtual machine"
          consequence={deletionConsequence(subject, volumes.items)}
          onClose={panel.close}
        />
      )}
    </div>
  );
}
