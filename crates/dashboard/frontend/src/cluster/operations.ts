import { create } from "./client";
import type { OperationSpecification, Step, VmReference } from "./kinds";

const LONGEST_SUBJECT = 40;
const STAMP_MASK = 0xff_ffff_ffff;

export const OPERATIONS = { plural: "operations", project: null };

export function uniqueName(prefix: string, subject: string): string {
  const stamp = Date.now() % (STAMP_MASK + 1);
  return `${prefix}-${subject.slice(0, LONGEST_SUBJECT)}-${stamp.toString(16)}`;
}

function operation(
  kind: OperationSpecification["kind"],
  target: OperationSpecification["target"],
  steps: Step[],
): OperationSpecification {
  return { kind, target, steps, is_paused: false, is_cancelled: false };
}

export function migration(vm: VmReference, destination: string | null): OperationSpecification {
  const prepare: Step = destination
    ? { step: "prepare-destination", vm, destination }
    : { step: "prepare-destination", vm };
  return operation("migration", { kind: "vm", vm }, [
    prepare,
    { step: "copy-memory", vm },
    { step: "switch-over", vm },
    { step: "announce-addresses", vm },
    { step: "clean-up-source", vm },
  ]);
}

export function evacuation(node: string, vms: VmReference[]): OperationSpecification {
  return operation("evacuation", { kind: "node", node }, [
    { step: "cordon", node },
    ...vms.map((vm): Step => ({ step: "migrate-vm", vm })),
  ]);
}

export function startMigration(vm: VmReference, destination: string | null): Promise<unknown> {
  return create(OPERATIONS, uniqueName("migrate", vm.name), migration(vm, destination));
}

export function startEvacuation(node: string, vms: VmReference[]): Promise<unknown> {
  return create(OPERATIONS, uniqueName("evacuate", node), evacuation(node, vms));
}
