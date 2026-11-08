import { Card, MainTable, Notification } from "@canonical/react-components";
import { Link } from "react-router";

import type { ClusterVersion, Operation, StorageBackend, Volume } from "../cluster/kinds";
import { isContainer } from "../cluster/kinds";
import { useCollection } from "../cluster/live";
import { OPERATIONS } from "../cluster/operations";
import Meter from "../components/Meter";
import PageHeader from "../components/PageHeader";
import Status from "../components/Status";
import Tag from "../components/Tag";
import { countOf, gibibytes, mebibytes, sentence } from "../format";
import { useProject } from "../session";
import { useAllocatedVms, zonesOf } from "./allocation";
import { phaseOf, useNodes, useVms } from "./instances";
import { currentStepOf, operationPhase, targetOf } from "./Operations";

const RECENT_OPERATIONS = 6;

function SummaryCard({ title, value, detail, to }: { title: string; value: string | number; detail: string; to: string }) {
  return (
    <Card className="summary-card">
      <h2 className="p-text--small-caps">
        <Link to={to}>{title}</Link>
      </h2>
      <p className="summary-card__value">{value}</p>
      <p className="u-text--muted u-no-margin--bottom">{detail}</p>
    </Card>
  );
}

export default function Overview() {
  const project = useProject();
  const nodes = useNodes();
  const vms = useVms();
  const allocated = useAllocatedVms();
  const backends = useCollection<StorageBackend>({ plural: "storage-backends" });
  const volumes = useCollection<Volume>({ plural: "volumes", project });
  const operations = useCollection<Operation>(OPERATIONS);
  const versions = useCollection<ClusterVersion>({ plural: "cluster-versions" });

  const machines = vms.items.filter((vm) => !isContainer(vm));
  const containers = vms.items.filter(isContainer);
  const running = (list: typeof vms.items) => list.filter((vm) => phaseOf(vm) === "running").length;
  const readyNodes = nodes.items.filter((node) => node.status.phase === "ready").length;
  const zones = zonesOf(nodes.items, allocated.vms);
  const provisioned = volumes.items.reduce((total, volume) => total + volume.specification.size_gibibytes, 0);
  const unreachable = backends.items.filter((backend) => backend.status.phase === "unreachable");
  const isHealthy = nodes.state !== "live" || (readyNodes === nodes.items.length && unreachable.length === 0);
  const version = versions.items[0]?.specification.image.version;
  const upgrade = operations.items.find((operation) => operation.specification.kind === "upgrade" && !operation.status.outcome);
  const recent = [...operations.items]
    .sort((first, second) => Number(Boolean(first.status.outcome)) - Number(Boolean(second.status.outcome)))
    .slice(0, RECENT_OPERATIONS);

  return (
    <div className="page">
      <PageHeader
        title={
          <>
            Cluster {version && <Tag>{version}</Tag>}{" "}
            <Tag emphasis={isHealthy ? "information" : "plain"}>{isHealthy ? "Healthy" : "Degraded"}</Tag>
          </>
        }
      />
      {upgrade && (
        <Notification severity="information" title="Upgrade in progress">
          {currentStepOf(upgrade)}: {upgrade.status.completed_steps ?? 0} of {upgrade.specification.steps.length} steps. VMs
          live-migrate off each node before it reboots, so nothing restarts. <Link to="/operations">Follow it</Link>
        </Notification>
      )}
      {unreachable.map((backend) => (
        <Notification key={backend.metadata.name} severity="caution" title={`${backend.metadata.name} is unreachable`}>
          {backend.status.message ?? "Volumes on it cannot be created, attached or migrated."}
        </Notification>
      ))}
      <div className="summary-cards">
        <SummaryCard
          title="Nodes"
          to="/nodes"
          value={nodes.state === "forbidden" ? "—" : nodes.items.length}
          detail={nodes.state === "forbidden" ? "not visible to your role" : `${readyNodes} ready across ${countOf(zones.length, "zone")}`}
        />
        <SummaryCard
          title="Virtual machines"
          to="/virtual-machines"
          value={machines.length}
          detail={`${running(machines)} running, ${machines.length - running(machines)} not running`}
        />
        <SummaryCard title="Containers" to="/containers" value={containers.length} detail={`${running(containers)} running`} />
        <SummaryCard
          title="Storage"
          to="/storage"
          value={gibibytes(provisioned)}
          detail={`in ${countOf(volumes.items.length, "volume")} on ${countOf(backends.items.length, "backend")}`}
        />
      </div>
      {nodes.state === "live" && (
        <>
          <h2 className="p-heading--4">Capacity by zone</h2>
          {!allocated.isEveryProject && <p className="u-text--muted">Allocation counts only the VMs of your project.</p>}
          <MainTable
            responsive
            headers={[
              { content: "Zone" },
              { content: "Nodes" },
              { content: "Core members" },
              { content: "CPU allocated" },
              { content: "Memory allocated" },
              { content: "Status" },
            ]}
            rows={zones.map((zone) => ({
              key: zone.name,
              columns: [
                { content: zone.name },
                { content: countOf(zone.nodes.length, "node") },
                { content: zone.core.join(", ") || "—" },
                { content: <Meter used={zone.allocatedCpus} total={zone.cpus} label={`${zone.allocatedCpus} of ${zone.cpus} vCPU`} /> },
                {
                  content: (
                    <Meter
                      used={zone.allocatedMebibytes}
                      total={zone.mebibytes}
                      label={`${mebibytes(zone.allocatedMebibytes)} of ${mebibytes(zone.mebibytes)}`}
                    />
                  ),
                },
                {
                  content:
                    zone.readyNodes === zone.nodes.length ? (
                      <Status phase="ready" label="Healthy" />
                    ) : (
                      <Status phase="not-ready" label={`${zone.nodes.length - zone.readyNodes} not ready`} />
                    ),
                },
              ],
            }))}
          />
        </>
      )}
      <h2 className="p-heading--4">Recent operations</h2>
      <MainTable
        responsive
        emptyStateMsg={operations.state === "forbidden" ? "Your role may not read operations." : "No operations yet."}
        headers={[{ content: "Operation" }, { content: "Target" }, { content: "Progress" }, { content: "Status" }]}
        rows={recent.map((operation) => ({
          key: operation.metadata.name,
          columns: [
            { content: <Link to={`/operations?panel=detail&subject=${operation.metadata.name}`}>{sentence(operation.specification.kind)}</Link> },
            { content: targetOf(operation) },
            {
              content: `${operation.status.completed_steps ?? 0} of ${operation.specification.steps.length} steps${currentStepOf(operation) ? ` · ${currentStepOf(operation)}` : ""}`,
            },
            { content: <Status {...operationPhase(operation)} /> },
          ],
        }))}
      />
    </div>
  );
}
