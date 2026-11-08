import type { Node, Vm } from "../cluster/kinds";
import { useCollection } from "../cluster/live";
import { useProject } from "../session";

export interface Allocation {
  cpus: number;
  mebibytes: number;
  vms: Vm[];
}

export interface AllocatedVms {
  vms: Vm[];
  isEveryProject: boolean;
}

export function useAllocatedVms(): AllocatedVms {
  const project = useProject();
  const everyProject = useCollection<Vm>({ plural: "vms", everyProject: true });
  const ownProject = useCollection<Vm>({ plural: "vms", project });
  if (everyProject.state === "forbidden") {
    return { vms: ownProject.items, isEveryProject: false };
  }
  return { vms: everyProject.items, isEveryProject: true };
}

export function allocationOn(vms: Vm[], node: string): Allocation {
  const hosted = vms.filter((vm) => vm.status.node === node);
  return {
    cpus: hosted.reduce((total, vm) => total + vm.specification.cpu_count, 0),
    mebibytes: hosted.reduce((total, vm) => total + vm.specification.memory_mebibytes, 0),
    vms: hosted,
  };
}

export interface Zone {
  name: string;
  nodes: Node[];
  core: string[];
  cpus: number;
  allocatedCpus: number;
  mebibytes: number;
  allocatedMebibytes: number;
  readyNodes: number;
}

export function zonesOf(nodes: Node[], vms: Vm[]): Zone[] {
  const zones = new Map<string, Node[]>();
  for (const node of nodes) {
    const zone = node.specification.zone ?? "no zone";
    zones.set(zone, [...(zones.get(zone) ?? []), node]);
  }
  return [...zones.entries()]
    .sort(([first], [second]) => first.localeCompare(second))
    .map(([name, members]) => {
      const allocations = members.map((node) => allocationOn(vms, node.metadata.name));
      return {
        name,
        nodes: members,
        core: members.filter((node) => (node.specification.roles ?? []).includes("core")).map((node) => node.metadata.name),
        cpus: members.reduce((total, node) => total + (node.status.capacity?.cpu_count ?? 0), 0),
        allocatedCpus: allocations.reduce((total, allocation) => total + allocation.cpus, 0),
        mebibytes: members.reduce((total, node) => total + (node.status.capacity?.memory_mebibytes ?? 0), 0),
        allocatedMebibytes: allocations.reduce((total, allocation) => total + allocation.mebibytes, 0),
        readyNodes: members.filter((node) => node.status.phase === "ready").length,
      };
    });
}
