import { ConfirmationModal, Input, Notification, Select } from "@canonical/react-components";
import { useState } from "react";

import type { KubernetesCluster, KubernetesClusterSpecification, MachineShape, NodePool } from "../cluster/environments";
import { requestKubernetesChange } from "../cluster/environments";
import Command from "../components/Command";
import Panel, { SubmitFooter } from "../components/Panel";
import { useSubmission } from "../components/submission";
import { countOf } from "../format";
import { useProject } from "../session";
import { NumberField, PlacementFields, ShapeSelect, chosen, nameProblem, usePlacementChoices } from "./environmentForms";

const VERSIONS = ["1.34", "1.33", "1.32"];
const CONTROL_PLANE_SIZES = [1, 3, 5];
const MAXIMUM_POOL_NODES = 100;

export function membersOf(cluster: KubernetesCluster, pool?: string) {
  return (cluster.status.nodes ?? []).filter((node) => pool === undefined || node.member.pool === pool).map((node) => node.member);
}

export function CreateClusterPanel({ taken, onClose }: { taken: string[]; onClose: () => void }) {
  const project = useProject();
  const choices = usePlacementChoices();
  const [name, setName] = useState("");
  const [version, setVersion] = useState(VERSIONS[0]);
  const [controlPlane, setControlPlane] = useState(1);
  const [controlPlaneShape, setControlPlaneShape] = useState<MachineShape>("medium");
  const [workers, setWorkers] = useState(2);
  const [workerShape, setWorkerShape] = useState<MachineShape>("large");
  const [placement, setPlacement] = useState({ network: "", storageBackend: "", externalNetwork: "" });
  const { isBusy, problem, submit } = useSubmission();
  const where = chosen(placement, choices);
  const problemWithName = nameProblem(name, taken);
  const nodePools: Record<string, NodePool> = workers > 0 ? { general: { nodes: workers, shape: workerShape } } : {};
  const cluster: KubernetesClusterSpecification = {
    version,
    control_plane_nodes: controlPlane,
    control_plane_shape: controlPlaneShape,
    node_pools: nodePools,
    network: where.network,
    storage_backend: where.storageBackend,
    external_network: where.externalNetwork,
  };
  return (
    <Panel
      title="Create Kubernetes cluster"
      isWide
      onClose={onClose}
      problem={problem}
      onSubmit={() => void submit(() => requestKubernetesChange(project, name, { kind: "create", cluster }), onClose)}
      footer={<SubmitFooter label="Create cluster" isBusy={isBusy || Boolean(problemWithName)} onCancel={onClose} />}
    >
      <Input id="cluster-name" type="text" label="Name" required value={name} error={problemWithName} onChange={(event) => setName(event.target.value)} />
      <Select id="cluster-version" label="Kubernetes version" value={version} onChange={(event) => setVersion(event.target.value)} options={VERSIONS.map((value) => ({ value, label: value }))} />
      <Select
        id="cluster-control-plane"
        label="Control plane"
        value={controlPlane}
        onChange={(event) => setControlPlane(Number(event.target.value))}
        options={CONTROL_PLANE_SIZES.map((value) => ({ value, label: countOf(value, "node") }))}
      />
      <ShapeSelect id="cluster-control-plane-shape" label="Control plane size" value={controlPlaneShape} onChange={setControlPlaneShape} />
      <NumberField id="cluster-workers" label="Workers in pool general" value={workers} minimum={0} maximum={MAXIMUM_POOL_NODES} onChange={setWorkers} />
      <ShapeSelect id="cluster-worker-shape" label="Worker size" value={workerShape} onChange={setWorkerShape} />
      <PlacementFields prefix="cluster" placement={where} choices={choices} onChange={setPlacement} />
      <p className="u-text--muted">
        This creates {countOf(controlPlane + workers, "VM")}, a floating IP for the API and a security group. Sign-in uses
        Amafu accounts: your role in this project becomes a Kubernetes group.
      </p>
      <Command
        words={[
          "amafu", "kubernetes", "create", name || "NAME", "--version", version,
          "--control-plane-nodes", String(controlPlane), "--control-plane-shape", controlPlaneShape,
          workers > 0 && "--pool", workers > 0 && `general=${workers}:${workerShape}`,
          "--network", where.network || "NETWORK", "--storage-backend", where.storageBackend || "BACKEND",
          "--external-network", where.externalNetwork || "NETWORK",
        ]}
      />
    </Panel>
  );
}

export function UpgradeClusterPanel({ cluster, onClose }: { cluster: KubernetesCluster; onClose: () => void }) {
  const project = useProject();
  const newer = VERSIONS.filter((candidate) => candidate.localeCompare(cluster.specification.version, undefined, { numeric: true }) > 0);
  const [version, setVersion] = useState(newer[newer.length - 1] ?? "");
  const { isBusy, problem, submit } = useSubmission();
  const nodes = membersOf(cluster);
  return (
    <Panel
      title={`Upgrade ${cluster.metadata.name}`}
      onClose={onClose}
      problem={problem}
      onSubmit={() => void submit(() => requestKubernetesChange(project, cluster.metadata.name, { kind: "upgrade", version, nodes }), onClose)}
      footer={<SubmitFooter label="Upgrade" isBusy={isBusy || !version} onCancel={onClose} />}
    >
      {newer.length === 0 ? (
        <Notification severity="information">{cluster.metadata.name} runs the newest version Amafu offers.</Notification>
      ) : (
        <Select id="upgrade-version" label="Upgrade to" value={version} onChange={(event) => setVersion(event.target.value)} options={newer.map((value) => ({ value, label: value }))} />
      )}
      <p className="u-text--muted">
        Each of the {countOf(nodes.length, "node")} is replaced one at a time, control plane first: a new VM joins at the new
        version, then the old one is drained and removed.
      </p>
      <Command words={["amafu", "kubernetes", "upgrade", cluster.metadata.name, "--version", version || "VERSION"]} />
    </Panel>
  );
}

export function ScalePoolPanel({ cluster, pool, onClose }: { cluster: KubernetesCluster; pool: string; onClose: () => void }) {
  const project = useProject();
  const nodes = membersOf(cluster, pool);
  const [to, setTo] = useState(nodes.length);
  const { isBusy, problem, submit } = useSubmission();
  const effect =
    to > nodes.length
      ? `Adds ${countOf(to - nodes.length, "VM")}.`
      : to < nodes.length
        ? `Cordons and drains the newest ${countOf(nodes.length - to, "node")}, then deletes their VMs.`
        : "No change.";
  return (
    <Panel
      title={`Scale node pool ${pool}`}
      onClose={onClose}
      problem={problem}
      onSubmit={() => void submit(() => requestKubernetesChange(project, cluster.metadata.name, { kind: "scale-pool", pool, nodes, to }), onClose)}
      footer={<SubmitFooter label="Scale" isBusy={isBusy || to === nodes.length} onCancel={onClose} />}
    >
      <NumberField id="scale-to" label="Scale to" value={to} minimum={0} maximum={MAXIMUM_POOL_NODES} onChange={setTo} help={effect} />
      <Command words={["amafu", "kubernetes", "scale", cluster.metadata.name, "--pool", pool, "--nodes", String(to)]} />
    </Panel>
  );
}

export function AddPoolPanel({ cluster, onClose }: { cluster: KubernetesCluster; onClose: () => void }) {
  const project = useProject();
  const [pool, setPool] = useState("");
  const [nodePool, setNodePool] = useState<NodePool>({ nodes: 1, shape: "large" });
  const { isBusy, problem, submit } = useSubmission();
  const problemWithName = nameProblem(pool, Object.keys(cluster.specification.node_pools ?? {}));
  return (
    <Panel
      title={`Add a node pool to ${cluster.metadata.name}`}
      onClose={onClose}
      problem={problem}
      onSubmit={() => void submit(() => requestKubernetesChange(project, cluster.metadata.name, { kind: "add-pool", pool, node_pool: nodePool }), onClose)}
      footer={<SubmitFooter label="Add pool" isBusy={isBusy || Boolean(problemWithName)} onCancel={onClose} />}
    >
      <Input id="pool-name" type="text" label="Pool name" required value={pool} error={problemWithName} onChange={(event) => setPool(event.target.value)} />
      <NumberField id="pool-nodes" label="Nodes" value={nodePool.nodes} minimum={1} maximum={MAXIMUM_POOL_NODES} onChange={(nodes) => setNodePool({ ...nodePool, nodes })} />
      <ShapeSelect id="pool-shape" label="Node size" value={nodePool.shape} onChange={(shape) => setNodePool({ ...nodePool, shape })} />
      <Command
        words={["amafu", "kubernetes", "add-pool", cluster.metadata.name, "--pool", pool || "POOL", "--nodes", String(nodePool.nodes), "--shape", nodePool.shape]}
      />
    </Panel>
  );
}

export function ClusterChangeConfirmation({
  cluster,
  title,
  consequence,
  label,
  request,
  onClose,
}: {
  cluster: KubernetesCluster;
  title: string;
  consequence: string;
  label: string;
  request: Parameters<typeof requestKubernetesChange>[2];
  onClose: () => void;
}) {
  const project = useProject();
  const { isBusy, problem, submit } = useSubmission();
  return (
    <ConfirmationModal
      title={title}
      confirmButtonLabel={label}
      confirmButtonAppearance="negative"
      confirmButtonLoading={isBusy}
      onConfirm={() => void submit(() => requestKubernetesChange(project, cluster.metadata.name, request), onClose)}
      close={onClose}
    >
      {problem && (
        <Notification severity="negative" title="The cluster refused">
          {problem}
        </Notification>
      )}
      <p>{consequence}</p>
    </ConfirmationModal>
  );
}
