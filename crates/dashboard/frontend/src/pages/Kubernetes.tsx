import { Button, MainTable } from "@canonical/react-components";

import type { KubernetesCluster, KubernetesClusterChange, PlanStep } from "../cluster/environments";
import { KUBERNETES_CHANGES, KUBERNETES_CLUSTERS, describePlanStep, isRunning } from "../cluster/environments";
import { useCollection } from "../cluster/live";
import { projectScope } from "../cluster/scopes";
import CollectionNotice from "../components/CollectionNotice";
import Command from "../components/Command";
import IconAction from "../components/IconAction";
import Meter from "../components/Meter";
import PageHeader from "../components/PageHeader";
import Panel from "../components/Panel";
import PanelTabs from "../components/PanelTabs";
import Status from "../components/Status";
import StepPlan, { PlanProgress, planPhase } from "../components/StepPlan";
import SubjectLink from "../components/SubjectLink";
import { usePanel } from "../components/panels";
import type { PanelState } from "../components/panels";
import { countOf } from "../format";
import { useProject } from "../session";
import {
  AddPoolPanel,
  ClusterChangeConfirmation,
  CreateClusterPanel,
  ScalePoolPanel,
  UpgradeClusterPanel,
  membersOf,
} from "./kubernetesPanels";

function describer(cluster: string) {
  return (step: PlanStep) => describePlanStep(step, cluster);
}

function changesOf(changes: KubernetesClusterChange[], cluster: string): KubernetesClusterChange[] {
  return changes.filter((change) => change.specification.cluster === cluster);
}

function ChangeList({ changes }: { changes: KubernetesClusterChange[] }) {
  if (changes.length === 0) {
    return <p className="u-text--muted">No changes yet.</p>;
  }
  return (
    <MainTable
      headers={[{ content: "Change" }, { content: "Progress" }, { content: "Status" }]}
      rows={changes.map((change) => ({
        key: change.metadata.name,
        columns: [
          { content: change.specification.request.kind.replaceAll("-", " ") },
          { content: <PlanProgress plan={change} describe={describer(change.specification.cluster)} /> },
          { content: <Status {...planPhase(change)} /> },
        ],
      }))}
    />
  );
}

function NodePools({ cluster, panel, isBusy }: { cluster: KubernetesCluster; panel: PanelState; isBusy: boolean }) {
  const name = cluster.metadata.name;
  return (
    <>
      <MainTable
        emptyStateMsg="No node pools; workloads run on the control plane."
        headers={[{ content: "Pool" }, { content: "Nodes" }, { content: "Size" }, { content: "Actions", className: "u-align--right" }]}
        rows={Object.entries(cluster.specification.node_pools ?? {}).map(([pool, nodePool]) => ({
          key: pool,
          columns: [
            { content: pool },
            { content: `${membersOf(cluster, pool).length} of ${nodePool.nodes}` },
            { content: nodePool.shape },
            {
              className: "u-align--right",
              content: (
                <div className="row-actions">
                  <IconAction icon="settings" label={`Scale ${pool}`} isDisabled={isBusy} onClick={() => panel.open("scale", `${name}/${pool}`)} />
                  <IconAction icon="delete" label={`Remove pool ${pool}`} isDisabled={isBusy} onClick={() => panel.open("remove-pool", `${name}/${pool}`)} />
                </div>
              ),
            },
          ],
        }))}
      />
      <Button type="button" disabled={isBusy} onClick={() => panel.open("add-pool", name)}>
        Add node pool
      </Button>
    </>
  );
}

function Nodes({ cluster, panel, isBusy }: { cluster: KubernetesCluster; panel: PanelState; isBusy: boolean }) {
  const name = cluster.metadata.name;
  return (
    <>
      <MainTable
        emptyStateMsg="No nodes yet."
        headers={[{ content: "Node" }, { content: "Role" }, { content: "Version" }, { content: "Status" }, { content: "Actions", className: "u-align--right" }]}
        rows={(cluster.status.nodes ?? []).map((node) => ({
          key: node.vm,
          columns: [
            { content: <SubjectLink name={node.vm} page="/virtual-machines" /> },
            { content: node.member.pool ? `worker · ${node.member.pool}` : "control plane" },
            { content: node.version },
            { content: <Status phase={node.is_ready ? "ready" : "pending"} /> },
            {
              className: "u-align--right",
              content: (
                <div className="row-actions">
                  <IconAction icon="pause" label={`Drain ${node.vm}`} isDisabled={isBusy} onClick={() => panel.open("drain", `${name}/${node.vm}`)} />
                  <IconAction icon="restart" label={`Restart VM ${node.vm}`} isDisabled={isBusy} onClick={() => panel.open("restart", `${name}/${node.vm}`)} />
                </div>
              ),
            },
          ],
        }))}
      />
    </>
  );
}

function ClusterDetail({
  cluster,
  changes,
  panel,
}: {
  cluster: KubernetesCluster;
  changes: KubernetesClusterChange[];
  panel: PanelState;
}) {
  const running = changes.find(isRunning);
  const name = cluster.metadata.name;
  return (
    <Panel title={name} isWide onClose={panel.close}>
      <p>
        Canonical Kubernetes {cluster.specification.version} · API {cluster.status.endpoint ?? "not reachable yet"}
      </p>
      <Status phase={cluster.status.phase} detail={cluster.status.message} />
      <PanelTabs
        tabs={[
          {
            label: "Overview",
            content: (
              <>
                {running ? (
                  <>
                    <h3 className="p-heading--5">Running: {running.specification.request.kind.replaceAll("-", " ")}</h3>
                    <StepPlan plan={running} describe={describer(name)} />
                  </>
                ) : (
                  <p className="u-text--muted">No change is running.</p>
                )}
                {(cluster.status.hosted_juju_controllers ?? []).length > 0 && (
                  <p>Hosts the Juju controllers {cluster.status.hosted_juju_controllers?.join(", ")}.</p>
                )}
                <Command title={`Use ${name} from kubectl`} words={["amafu", "kubernetes", "kubeconfig", name]} />
                <p className="u-text--muted">
                  The kubeconfig holds no secret: kubectl asks <code>amafu kubernetes token {name}</code> for a five-minute token
                  signed by this cluster, and your role in the project decides what you may do.
                </p>
              </>
            ),
          },
          { label: `Node pools (${Object.keys(cluster.specification.node_pools ?? {}).length})`, content: <NodePools cluster={cluster} panel={panel} isBusy={Boolean(running)} /> },
          { label: `Nodes (${cluster.status.nodes?.length ?? 0})`, content: <Nodes cluster={cluster} panel={panel} isBusy={Boolean(running)} /> },
          { label: `Changes (${changes.length})`, content: <ChangeList changes={changes} /> },
        ]}
      />
    </Panel>
  );
}

function split(subject: string | null): [string, string] {
  const [cluster, rest] = (subject ?? "").split("/");
  return [cluster ?? "", rest ?? ""];
}

function ChangePanels({ clusters, panel }: { clusters: KubernetesCluster[]; panel: PanelState }) {
  const [clusterName, part] = split(panel.subject);
  const cluster = clusters.find((candidate) => candidate.metadata.name === clusterName);
  if (!cluster) {
    return null;
  }
  const node = (cluster.status.nodes ?? []).find((candidate) => candidate.vm === part);
  switch (panel.panel) {
    case "upgrade":
      return <UpgradeClusterPanel cluster={cluster} onClose={panel.close} />;
    case "scale":
      return <ScalePoolPanel cluster={cluster} pool={part} onClose={panel.close} />;
    case "add-pool":
      return <AddPoolPanel cluster={cluster} onClose={panel.close} />;
    case "remove-pool":
      return (
        <ClusterChangeConfirmation
          cluster={cluster}
          title={`Remove node pool ${part}?`}
          label="Remove pool"
          consequence={`Its ${countOf(membersOf(cluster, part).length, "node")} are drained, removed from Kubernetes and deleted.`}
          request={{ kind: "remove-pool", pool: part, nodes: membersOf(cluster, part) }}
          onClose={panel.close}
        />
      );
    case "drain":
      return node ? (
        <ClusterChangeConfirmation
          cluster={cluster}
          title={`Drain ${node.vm}?`}
          label="Drain"
          consequence="The node is cordoned and its pods are evicted to other nodes. It stays cordoned until it restarts."
          request={{ kind: "drain-node", member: node.member }}
          onClose={panel.close}
        />
      ) : null;
    case "restart":
      return node ? (
        <ClusterChangeConfirmation
          cluster={cluster}
          title={`Restart ${node.vm}?`}
          label="Restart"
          consequence="The node is drained, its VM is stopped and started, and it is uncordoned once it is ready."
          request={{ kind: "restart-node", member: node.member }}
          onClose={panel.close}
        />
      ) : null;
    case "delete":
      return (
        <ClusterChangeConfirmation
          cluster={cluster}
          title={`Delete ${cluster.metadata.name}?`}
          label="Delete cluster"
          consequence={`This deletes its ${countOf(cluster.status.nodes?.length ?? 0, "VM")}, their volumes, its floating IP and its security group. A cluster that hosts a Juju controller cannot be deleted.`}
          request={{ kind: "delete" }}
          onClose={panel.close}
        />
      );
    default:
      return null;
  }
}

export default function Kubernetes() {
  const project = useProject();
  const clusters = useCollection<KubernetesCluster>(projectScope(KUBERNETES_CLUSTERS, project));
  const changes = useCollection<KubernetesClusterChange>(projectScope(KUBERNETES_CHANGES, project));
  const panel = usePanel();
  const subject = clusters.items.find((cluster) => cluster.metadata.name === panel.subject);
  const creating = changes.items.filter(
    (change) => change.specification.request.kind === "create" && isRunning(change) && !clusters.items.some((cluster) => cluster.metadata.name === change.specification.cluster),
  );
  return (
    <div className="page">
      <PageHeader title="Kubernetes">
        <Button appearance="positive" onClick={() => panel.open("create")}>
          Create cluster
        </Button>
      </PageHeader>
      <p className="u-text--muted">
        Canonical Kubernetes clusters on Amafu VMs. Amafu creates, scales, upgrades and deletes them; kubectl signs in with
        your Amafu account.
      </p>
      <CollectionNotice state={clusters.state} noun="Kubernetes clusters" />
      <MainTable
        responsive
        emptyStateMsg="No Kubernetes clusters. Create one to run workloads or Juju on Kubernetes."
        headers={[
          { content: "Name" },
          { content: "Version" },
          { content: "Status" },
          { content: "Nodes ready" },
          { content: "API" },
          { content: "Used by" },
          { content: "Actions", className: "u-align--right" },
        ]}
        rows={[
          ...creating.map((change) => ({
            key: `creating-${change.metadata.name}`,
            columns: [
              { content: change.specification.cluster },
              { content: change.specification.request.kind === "create" ? change.specification.request.cluster.version : "" },
              { content: <Status phase="creating" detail={planPhase(change).detail} /> },
              { content: <PlanProgress plan={change} describe={describer(change.specification.cluster)} /> },
              { content: "—" },
              { content: "—" },
              { content: "" },
            ],
          })),
          ...clusters.items.map((cluster) => {
            const nodes = cluster.status.nodes ?? [];
            const ready = nodes.filter((node) => node.is_ready).length;
            const isChanging = changesOf(changes.items, cluster.metadata.name).some(isRunning);
            return {
              key: cluster.metadata.name,
              columns: [
                { content: <SubjectLink name={cluster.metadata.name} /> },
                { content: cluster.specification.version },
                { content: <Status phase={cluster.status.phase} detail={cluster.status.message} /> },
                { content: <Meter used={ready} total={nodes.length} label={`${ready} of ${nodes.length}`} /> },
                { content: cluster.status.endpoint ?? "—" },
                { content: (cluster.status.hosted_juju_controllers ?? []).join(", ") || "—" },
                {
                  className: "u-align--right",
                  content: (
                    <div className="row-actions">
                      <IconAction icon="change-version" label={`Upgrade ${cluster.metadata.name}`} isDisabled={isChanging} onClick={() => panel.open("upgrade", cluster.metadata.name)} />
                      <IconAction icon="delete" label={`Delete ${cluster.metadata.name}`} isDisabled={isChanging} onClick={() => panel.open("delete", cluster.metadata.name)} />
                    </div>
                  ),
                },
              ],
            };
          }),
        ]}
      />
      {panel.panel === "create" && <CreateClusterPanel taken={clusters.items.map((cluster) => cluster.metadata.name)} onClose={panel.close} />}
      {panel.panel === "detail" && subject && (
        <ClusterDetail cluster={subject} changes={changesOf(changes.items, subject.metadata.name)} panel={panel} />
      )}
      <ChangePanels clusters={clusters.items} panel={panel} />
    </div>
  );
}

