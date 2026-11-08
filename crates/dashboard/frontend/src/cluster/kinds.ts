export interface Metadata {
  id: string;
  name: string;
  project?: string;
  generation: number;
  observed_generation: number;
  is_deleting: boolean;
  version: number;
  labels?: Record<string, string>;
}

export interface ClusterObject<Specification, Status> {
  metadata: Metadata;
  specification: Specification;
  status: Status;
}

export type PowerState = "running" | "stopped";

export type BootSource =
  | { type: "kernel"; bundle: string }
  | { type: "disk" }
  | { type: "container"; image: string; command?: string[]; environment?: Record<string, string> };

export interface NetworkInterfaceSpecification {
  network: string;
  security_groups?: string[];
  address?: string;
}

export interface VmSpecification {
  cpu_count: number;
  memory_mebibytes: number;
  boot: BootSource;
  volumes?: string[];
  network_interfaces?: NetworkInterfaceSpecification[];
  power_state?: PowerState;
  user_data?: string;
  priority?: number;
  anti_affinity_group?: string;
  minimum_cpu_baseline?: string;
}

export type VmPhase = "pending" | "scheduled" | "starting" | "running" | "stopping" | "stopped" | "failed";

export type MigrationPhase =
  | "preparing"
  | "destination-ready"
  | "copying"
  | "copied"
  | "switched-over"
  | "cleaned"
  | "failed";

export interface VmMigration {
  source: string;
  destination: string;
  phase: MigrationPhase;
  downtime_milliseconds: number;
  report?: { total_bytes: number; duration_milliseconds: number; downtime_milliseconds: number };
  message?: string;
}

export interface VmStatus {
  phase?: VmPhase;
  node?: string;
  interfaces?: { network: string; mac: string; address: string }[];
  message?: string;
  migration?: VmMigration;
}

export type Vm = ClusterObject<VmSpecification, VmStatus>;

export type ImageSource = { type: "disk"; url: string; sha256: string } | { type: "container"; reference: string };

export type Image = ClusterObject<
  { source: ImageSource },
  { phase?: "pending" | "ready" | "failed"; size_bytes?: number; cached_on?: string[]; message?: string }
>;

export type Volume = ClusterObject<
  { size_gibibytes: number; backend: string; source_image?: string },
  {
    phase?: "pending" | "available" | "attached" | "failed";
    provisioned?: { identifier: string; size_bytes: number; node?: string };
    attached_to?: string;
    message?: string;
  }
>;

export type Network = ClusterObject<
  { subnet: string; is_outbound_access_allowed: boolean },
  { phase?: "pending" | "ready" | "failed"; gateway?: string; segment_id?: number; message?: string }
>;

export type ExternalNetwork = ClusterObject<{ subnet: string; first_address: string; last_address: string }, object>;

export type Direction = "ingress" | "egress";
export type Protocol = "any" | "tcp" | "udp" | "icmp";
export type Remote = { type: "any" } | { type: "addresses"; network: string } | { type: "security-group"; name: string };

export interface SecurityGroupRule {
  direction: Direction;
  protocol: Protocol;
  ports?: { first: number; last: number };
  remote: Remote;
}

export type SecurityGroup = ClusterObject<{ rules: SecurityGroupRule[] }, object>;

export type FloatingIp = ClusterObject<
  { external_network: string; address?: string; target?: { vm: string; interface: number } },
  {
    phase?: "pending" | "allocated" | "bound" | "failed";
    address?: string;
    binding?: { node: string; private_address: string };
    message?: string;
  }
>;

export type NodeRole = "core" | "compute";
export type NodePhase = "unknown" | "ready" | "not-ready" | "suspect" | "lost";

export interface ImageIdentity {
  version: string;
  digest: string;
}

export interface BootReport {
  booted_slot: "a" | "b";
  default_slot: "a" | "b";
  booted_image?: ImageIdentity;
  inactive_image?: ImageIdentity;
  fallen_back_from?: ImageIdentity;
  refused?: { image: ImageIdentity; reason: string };
}

export type Node = ClusterObject<
  { roles?: NodeRole[]; is_cordoned?: boolean; zone?: string },
  {
    phase?: NodePhase;
    capacity?: { cpu_count: number; memory_mebibytes: number };
    cpu_baseline?: string;
    underlay_address?: string;
    software_version?: string;
    last_heartbeat?: string;
    boot?: BootReport;
  }
>;

export type StorageProvider =
  | { type: "local-disk" }
  | { type: "nfs"; server: string; export: string }
  | { type: "nvme-tcp"; management: string; addresses: string[]; pool: string }
  | { type: "ceph-rbd"; monitors: string[]; pool: string; user: string; management: string }
  | {
      type: "plugin";
      module: { url: string; digest: string };
      configuration: unknown;
      endpoints?: string[];
      secrets?: Record<string, string>;
    };

export type StorageBackend = ClusterObject<
  { provider: StorageProvider },
  { phase?: "unknown" | "ready" | "unreachable"; message?: string }
>;

export interface VmReference {
  project: string;
  name: string;
}

export type OperationTarget = { kind: "node"; node: string } | { kind: "vm"; vm: VmReference } | { kind: "cluster" };

export type Step = { step: string; node?: string; vm?: VmReference; destination?: string };

export type OperationOutcome = { outcome: "succeeded" } | { outcome: "failed"; reason: string } | { outcome: "cancelled" };

export interface OperationSpecification {
  kind: "migration" | "recovery" | "evacuation" | "upgrade" | "install";
  target: OperationTarget;
  steps: Step[];
  is_paused?: boolean;
  is_cancelled?: boolean;
}

export type Operation = ClusterObject<
  OperationSpecification,
  {
    completed_steps?: number;
    current_step?: { attempts: number; last_error?: string; started_at: string };
    rollback?: { remaining_steps: number };
    outcome?: OperationOutcome;
  }
>;

export type ClusterVersion = ClusterObject<
  {
    image: { version: string; feature_level: number; digest: string };
    previous_image?: { version: string };
    finalized_feature_level: number;
  },
  object
>;

export type AuditSinkSpecification =
  | { type: "syslog"; address: string; server_name?: string; certificate_authority?: string }
  | {
      type: "object-storage";
      endpoint: string;
      bucket: string;
      region: string;
      prefix: string;
      access_key: string;
      secret_key: string;
    };

export type AuditSink = ClusterObject<AuditSinkSpecification, { next_sequence?: number; last_failure?: string }>;

export function isContainer(vm: Vm): boolean {
  return vm.specification.boot.type === "container";
}
