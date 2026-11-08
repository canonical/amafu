import { ConfirmationModal, MainTable, Notification } from "@canonical/react-components";
import { useState } from "react";

import { update } from "../cluster/client";
import type { Node } from "../cluster/kinds";
import { startEvacuation } from "../cluster/operations";
import { clusterScope } from "../cluster/scopes";
import CollectionNotice from "../components/CollectionNotice";
import Command from "../components/Command";
import IconAction from "../components/IconAction";
import Meter from "../components/Meter";
import PageHeader from "../components/PageHeader";
import Panel from "../components/Panel";
import SearchHeader, { matches } from "../components/SearchHeader";
import Status from "../components/Status";
import SubjectLink from "../components/SubjectLink";
import Tag from "../components/Tag";
import { usePanel } from "../components/panels";
import { useSubmission } from "../components/submission";
import { mebibytes, sentence, shortDigest, sinceNow } from "../format";
import { useProject } from "../session";
import { allocationOn, useAllocatedVms } from "./allocation";
import { DetailList, referenceOf, useNodes } from "./instances";

function nodePhase(node: Node): { phase: string; label?: string; detail?: string } {
  const phase = node.status.phase ?? "unknown";
  if (node.specification.is_cordoned) {
    return { phase: "cordoned", label: "Cordoned", detail: sentence(phase) };
  }
  return { phase };
}

function versionOf(node: Node): string {
  return node.status.boot?.booted_image?.version ?? node.status.software_version ?? "—";
}

function setCordoned(node: Node, isCordoned: boolean): Promise<unknown> {
  return update(clusterScope("nodes"), node.metadata.name, { ...node.specification, is_cordoned: isCordoned }, node.metadata.generation);
}

function NodeDetail({ node, onClose }: { node: Node; onClose: () => void }) {
  const boot = node.status.boot;
  return (
    <Panel title={node.metadata.name} onClose={onClose}>
      {boot?.refused && (
        <Notification severity="negative" title={`Refused ${boot.refused.image.version}`}>
          {boot.refused.reason}
        </Notification>
      )}
      {boot?.fallen_back_from && (
        <Notification severity="caution" title="Fell back">
          The node could not confirm {boot.fallen_back_from.version} and booted its previous image again.
        </Notification>
      )}
      <DetailList
        rows={[
          { label: "Status", value: <Status {...nodePhase(node)} /> },
          { label: "Roles", value: (node.specification.roles ?? []).join(", ") },
          { label: "Zone", value: node.specification.zone },
          { label: "Underlay", value: node.status.underlay_address },
          { label: "CPU baseline", value: node.status.cpu_baseline },
          { label: "Capacity", value: node.status.capacity ? `${node.status.capacity.cpu_count} vCPU, ${mebibytes(node.status.capacity.memory_mebibytes)}` : undefined },
          { label: "Software", value: node.status.software_version },
          { label: "Booted", value: boot?.booted_image ? `${boot.booted_image.version} from slot ${boot.booted_slot}` : undefined },
          { label: "Digest", value: boot?.booted_image ? shortDigest(boot.booted_image.digest) : undefined },
          { label: "Default slot", value: boot?.default_slot },
          { label: "Other slot", value: boot?.inactive_image?.version },
          { label: "Last heartbeat", value: sinceNow(node.status.last_heartbeat) },
        ]}
      />
      <Command words={["amafu", "node", "show", node.metadata.name]} />
    </Panel>
  );
}

function EvacuateConfirmation({ node, onClose }: { node: Node; onClose: () => void }) {
  const project = useProject();
  const allocated = useAllocatedVms();
  const { isBusy, problem, submit } = useSubmission();
  const hosted = allocationOn(allocated.vms, node.metadata.name).vms;
  return (
    <ConfirmationModal
      title={`Evacuate ${node.metadata.name}?`}
      confirmButtonLabel="Evacuate"
      confirmButtonAppearance="negative"
      confirmButtonLoading={isBusy}
      onConfirm={() =>
        void submit(() => startEvacuation(node.metadata.name, hosted.map((vm) => referenceOf(vm, project))), onClose)
      }
      close={onClose}
    >
      {problem && (
        <Notification severity="negative" title="The cluster refused">
          {problem}
        </Notification>
      )}
      {!allocated.isEveryProject && (
        <Notification severity="caution" title="Only your project is visible">
          VMs of other projects on this node are not moved; ask an admin to evacuate it.
        </Notification>
      )}
      <p>
        The node is cordoned, then its {hosted.length} VMs live-migrate one at a time. It stays cordoned afterwards.
      </p>
      <Command words={["amafu", "node", "evacuate", node.metadata.name]} />
    </ConfirmationModal>
  );
}

export default function Nodes() {
  const nodes = useNodes();
  const allocated = useAllocatedVms();
  const panel = usePanel();
  const actions = useSubmission();
  const [query, setQuery] = useState("");
  const shown = nodes.items.filter((node) => matches(query, node.metadata.name, node.specification.zone, versionOf(node)));
  const subject = nodes.items.find((node) => node.metadata.name === panel.subject);

  return (
    <div className="page">
      <PageHeader title="Nodes">
        <SearchHeader noun="nodes" value={query} onChange={setQuery} />
      </PageHeader>
      <CollectionNotice state={nodes.state} noun="nodes" />
      {!allocated.isEveryProject && nodes.state === "live" && (
        <p className="u-text--muted">Allocation counts only the VMs of your project.</p>
      )}
      {actions.problem && (
        <Notification severity="negative" title="The cluster refused" onDismiss={actions.clear}>
          {actions.problem}
        </Notification>
      )}
      <MainTable
        sortable
        responsive
        emptyStateMsg="No nodes."
        headers={[
          { content: "Name", sortKey: "name" },
          { content: "Zone", sortKey: "zone" },
          { content: "Status", sortKey: "status" },
          { content: "CPU allocated" },
          { content: "Memory allocated" },
          { content: "VMs", sortKey: "vms", className: "u-align--right" },
          { content: "Version", sortKey: "version" },
          { content: "Heartbeat" },
          { content: "Actions", className: "u-align--right" },
        ]}
        rows={shown.map((node) => {
          const allocation = allocationOn(allocated.vms, node.metadata.name);
          const capacity = node.status.capacity ?? { cpu_count: 0, memory_mebibytes: 0 };
          return {
            key: node.metadata.name,
            columns: [
              {
                content: (
                  <>
                    <SubjectLink name={node.metadata.name} />
                    <div className="tag-list">
                      {(node.specification.roles ?? []).map((role) => (
                        <Tag key={role} emphasis={role === "core" ? "information" : "plain"}>
                          {sentence(role)}
                        </Tag>
                      ))}
                    </div>
                  </>
                ),
              },
              { content: node.specification.zone ?? "—" },
              { content: <Status {...nodePhase(node)} /> },
              { content: <Meter used={allocation.cpus} total={capacity.cpu_count} label={`${allocation.cpus} of ${capacity.cpu_count} vCPU`} /> },
              {
                content: (
                  <Meter
                    used={allocation.mebibytes}
                    total={capacity.memory_mebibytes}
                    label={`${mebibytes(allocation.mebibytes)} of ${mebibytes(capacity.memory_mebibytes)}`}
                  />
                ),
              },
              { content: allocation.vms.length, className: "u-align--right" },
              { content: versionOf(node) },
              { content: sinceNow(node.status.last_heartbeat) },
              {
                content: (
                  <div className="row-actions">
                    <IconAction
                      icon={node.specification.is_cordoned ? "lock-unlock" : "lock-locked"}
                      label={node.specification.is_cordoned ? `Uncordon ${node.metadata.name}` : `Cordon ${node.metadata.name}`}
                      onClick={() => void actions.submit(() => setCordoned(node, !node.specification.is_cordoned))}
                    />
                    <IconAction icon="export" label={`Evacuate ${node.metadata.name}`} onClick={() => panel.open("evacuate", node.metadata.name)} />
                  </div>
                ),
                className: "u-align--right",
              },
            ],
            sortData: {
              name: node.metadata.name,
              zone: node.specification.zone ?? "",
              status: nodePhase(node).phase,
              vms: allocation.vms.length,
              version: versionOf(node),
            },
          };
        })}
      />
      {panel.panel === "detail" && subject && <NodeDetail node={subject} onClose={panel.close} />}
      {panel.panel === "evacuate" && subject && <EvacuateConfirmation node={subject} onClose={panel.close} />}
    </div>
  );
}
