import { create } from "./client";
import type { ClusterObject, Operation } from "./kinds";
import { uniqueName } from "./operations";
import { projectScope } from "./scopes";

export type MachineShape = "small" | "medium" | "large" | "xlarge";

export const SHAPES: { value: MachineShape; label: string }[] = [
  { value: "small", label: "Small · 2 vCPU · 4 GiB · 20 GiB" },
  { value: "medium", label: "Medium · 4 vCPU · 8 GiB · 40 GiB" },
  { value: "large", label: "Large · 8 vCPU · 16 GiB · 80 GiB" },
  { value: "xlarge", label: "Extra large · 16 vCPU · 32 GiB · 160 GiB" },
];

export type EnvironmentPhase = "creating" | "ready" | "changing" | "degraded" | "deleting" | "failed";

export type PlanStatus = Operation["status"];

export interface PlanStep {
  step: string;
  member?: ClusterMember;
  model?: string;
  application?: string;
  version?: string;
}

export interface StepPlanSpecification {
  steps: PlanStep[];
  is_paused?: boolean;
  is_cancelled?: boolean;
}

export interface ClusterMember {
  pool?: string;
  index: number;
}

export interface NodePool {
  nodes: number;
  shape: MachineShape;
}

export interface KubernetesClusterSpecification {
  version: string;
  control_plane_nodes: number;
  control_plane_shape: MachineShape;
  node_pools?: Record<string, NodePool>;
  network: string;
  storage_backend: string;
  external_network: string;
  organization_sign_in?: boolean;
}

export interface KubernetesNode {
  member: ClusterMember;
  vm: string;
  version: string;
  is_ready?: boolean;
}

export type KubernetesCluster = ClusterObject<
  KubernetesClusterSpecification,
  {
    phase?: EnvironmentPhase;
    endpoint?: string;
    uid?: string;
    certificate_authority?: string;
    nodes?: KubernetesNode[];
    hosted_juju_controllers?: string[];
    message?: string;
  }
>;

export type KubernetesRequest =
  | { kind: "create"; cluster: KubernetesClusterSpecification }
  | { kind: "delete" }
  | { kind: "upgrade"; version: string; nodes: ClusterMember[] }
  | { kind: "scale-pool"; pool: string; nodes: ClusterMember[]; to: number }
  | { kind: "add-pool"; pool: string; node_pool: NodePool }
  | { kind: "remove-pool"; pool: string; nodes: ClusterMember[] }
  | { kind: "drain-node"; member: ClusterMember }
  | { kind: "restart-node"; member: ClusterMember };

export type KubernetesClusterChange = ClusterObject<
  StepPlanSpecification & { cluster: string; request: KubernetesRequest },
  PlanStatus
>;

export type Substrate =
  | {
      on: "pods";
      pods: number;
      controllers: number;
      shape: MachineShape;
      network: string;
      storage_backend: string;
      external_network: string;
    }
  | { on: "kubernetes"; cluster: string };

export interface JujuControllerSpecification {
  version: string;
  substrate: Substrate;
  first_model: string;
  sign_in?: "amafu" | "jaas";
}

export type JujuController = ClusterObject<
  JujuControllerSpecification,
  {
    phase?: EnvironmentPhase;
    uuid?: string;
    addresses?: string[];
    models?: { name: string; uuid: string }[];
    version?: string;
    message?: string;
  }
>;

export interface Deployment {
  model: string;
  application: string;
  charm: string;
  channel?: string;
  units: number;
  expose?: boolean;
}

export type JujuRequest =
  | { kind: "create"; controller: JujuControllerSpecification }
  | { kind: "delete" }
  | { kind: "upgrade"; version: string }
  | { kind: "add-model"; model: string }
  | { kind: "destroy-model"; model: string }
  | { kind: "enable-ha" }
  | { kind: "deploy"; deployment: Deployment }
  | { kind: "scale-pods"; pods: number }
  | { kind: "scale-application"; model: string; application: string; units: number }
  | { kind: "remove-units"; model: string; application: string; units: string[] }
  | { kind: "remove-application"; model: string; application: string }
  | { kind: "rotate-credential" };

export type JujuControllerChange = ClusterObject<
  StepPlanSpecification & { controller: string; request: JujuRequest },
  PlanStatus
>;

export const KUBERNETES_CLUSTERS = "kubernetes-clusters";
export const KUBERNETES_CHANGES = "kubernetes-cluster-changes";
export const JUJU_CONTROLLERS = "juju-controllers";
export const JUJU_CHANGES = "juju-controller-changes";
export const KUBERNETES_LABEL = "amafu.io/kubernetes-cluster";
export const JUJU_LABEL = "amafu.io/juju-controller";
export const MAXIMUM_PODS = 16;

export function isValidPodCount(pods: number): boolean {
  return pods === 1 || (pods >= 3 && pods <= MAXIMUM_PODS);
}

export function requestKubernetesChange(project: string | null, cluster: string, request: KubernetesRequest): Promise<unknown> {
  return create(projectScope(KUBERNETES_CHANGES, project), uniqueName(request.kind, cluster), { cluster, request });
}

export function requestJujuChange(project: string | null, controller: string, request: JujuRequest): Promise<unknown> {
  return create(projectScope(JUJU_CHANGES, project), uniqueName(request.kind, controller), { controller, request });
}

export function memberName(cluster: string, member: ClusterMember): string {
  return member.pool ? `${cluster}-${member.pool}-${member.index}` : `${cluster}-cp-${member.index}`;
}

const ACRONYMS: Record<string, string> = { api: "API", ha: "HA", dns: "DNS", ca: "CA", vm: "VM", vms: "VMs" };

export function describePlanStep(step: PlanStep, cluster = ""): string {
  const words = step.step.split("-").map((word) => ACRONYMS[word] ?? word).join(" ");
  const action = words.charAt(0).toUpperCase() + words.slice(1);
  const subject = step.member
    ? memberName(cluster, step.member)
    : (step.application ?? step.model ?? step.version ?? "");
  return subject ? `${action} ${subject}` : action;
}

export function isRunning(plan: { status: PlanStatus }): boolean {
  return !plan.status.outcome;
}
