import { Button, CheckboxInput, Input, MainTable, Tabs } from "@canonical/react-components";
import { useState } from "react";

import { create } from "../cluster/client";
import type { ExternalNetwork, FloatingIp, Network, Vm } from "../cluster/kinds";
import { useCollection } from "../cluster/live";
import { clusterScope, projectScope } from "../cluster/scopes";
import CollectionNotice from "../components/CollectionNotice";
import Command from "../components/Command";
import DeleteConfirmation from "../components/DeleteConfirmation";
import IconAction from "../components/IconAction";
import PageHeader from "../components/PageHeader";
import Panel, { SubmitFooter } from "../components/Panel";
import SearchHeader, { matches } from "../components/SearchHeader";
import Status from "../components/Status";
import Tag from "../components/Tag";
import { usePanel } from "../components/panels";
import { useSubmission } from "../components/submission";
import { countOf } from "../format";
import { useProject } from "../session";
import { useVms } from "./instances";

function CreateNetworkPanel({ onClose }: { onClose: () => void }) {
  const project = useProject();
  const [name, setName] = useState("");
  const [subnet, setSubnet] = useState("10.10.0.0/24");
  const [isIsolated, setIsolated] = useState(false);
  const { isBusy, problem, submit } = useSubmission();
  const specification = { subnet, is_outbound_access_allowed: !isIsolated };
  return (
    <Panel
      title="Create network"
      onClose={onClose}
      problem={problem}
      onSubmit={() => void submit(() => create(projectScope("networks", project), name, specification), onClose)}
      footer={<SubmitFooter label="Create" isBusy={isBusy} onCancel={onClose} />}
    >
      <Input type="text" id="network-name" label="Name" required value={name} onChange={(event) => setName(event.target.value)} />
      <Input type="text" id="network-subnet" label="Subnet" required value={subnet} onChange={(event) => setSubnet(event.target.value)} />
      <CheckboxInput
        label="Keep VMs on this network from reaching outside it"
        checked={isIsolated}
        onChange={() => setIsolated(!isIsolated)}
      />
      <Command words={["amafu", "network", "create", name || "NAME", "--subnet", subnet, isIsolated && "--no-outbound-access"]} />
    </Panel>
  );
}

function CreateExternalNetworkPanel({ onClose }: { onClose: () => void }) {
  const [name, setName] = useState("");
  const [subnet, setSubnet] = useState("");
  const [first, setFirst] = useState("");
  const [last, setLast] = useState("");
  const { isBusy, problem, submit } = useSubmission();
  const specification = { subnet, first_address: first, last_address: last };
  return (
    <Panel
      title="Declare external network"
      onClose={onClose}
      problem={problem}
      onSubmit={() => void submit(() => create(clusterScope("external-networks"), name, specification), onClose)}
      footer={<SubmitFooter label="Declare" isBusy={isBusy} onCancel={onClose} />}
    >
      <p className="u-text--muted">A range of addresses on the underlay that floating IPs are taken from.</p>
      <Input type="text" id="external-name" label="Name" required value={name} onChange={(event) => setName(event.target.value)} />
      <Input type="text" id="external-subnet" label="Subnet" placeholder="203.0.113.0/24" required value={subnet} onChange={(event) => setSubnet(event.target.value)} />
      <div className="field-pair">
        <Input type="text" id="external-first" label="First address" required value={first} onChange={(event) => setFirst(event.target.value)} />
        <Input type="text" id="external-last" label="Last address" required value={last} onChange={(event) => setLast(event.target.value)} />
      </div>
      <Command
        words={["amafu", "external-network", "create", name || "NAME", "--subnet", subnet || "CIDR", "--first", first || "ADDRESS", "--last", last || "ADDRESS"]}
      />
    </Panel>
  );
}

function instancesOn(vms: Vm[], network: string): number {
  return vms.filter((vm) => (vm.specification.network_interfaces ?? []).some((nic) => nic.network === network)).length;
}

function PrivateNetworks({ query }: { query: string }) {
  const project = useProject();
  const networks = useCollection<Network>({ plural: "networks", project });
  const vms = useVms();
  const panel = usePanel();
  const shown = networks.items.filter((network) => matches(query, network.metadata.name, network.specification.subnet));
  return (
    <>
      <CollectionNotice state={networks.state} noun="networks" />
      <MainTable
        sortable
        responsive
        emptyStateMsg="No networks yet."
        headers={[
          { content: "Name", sortKey: "name" },
          { content: "Type" },
          { content: "Subnet" },
          { content: "Gateway" },
          { content: "Outbound" },
          { content: "Segment", className: "u-align--right" },
          { content: "Instances", sortKey: "instances", className: "u-align--right" },
          { content: "Status" },
          { content: "Actions", className: "u-align--right" },
        ]}
        rows={shown.map((network) => ({
          key: network.metadata.name,
          columns: [
            { content: network.metadata.name },
            { content: <Tag>Overlay</Tag> },
            { content: <span className="u-text--monospace">{network.specification.subnet}</span> },
            {
              content: (
                <>
                  <span className="u-text--monospace">{network.status.gateway ?? "—"}</span>
                  {network.status.gateway && <div className="secondary-line">answered on every node</div>}
                </>
              ),
            },
            { content: network.specification.is_outbound_access_allowed ? "Allowed" : "Blocked" },
            { content: network.status.segment_id ?? "—", className: "u-align--right" },
            { content: instancesOn(vms.items, network.metadata.name), className: "u-align--right" },
            { content: <Status phase={network.status.phase ?? "pending"} detail={network.status.phase === "failed" ? network.status.message : undefined} /> },
            {
              content: <IconAction icon="delete" label={`Delete ${network.metadata.name}`} onClick={() => panel.open("delete", network.metadata.name)} />,
              className: "u-align--right",
            },
          ],
          sortData: { name: network.metadata.name, instances: instancesOn(vms.items, network.metadata.name) },
        }))}
      />
      <p className="u-text--muted footnote">
        Overlay networks are carried over Geneve. DHCP, DNS and the gateway are answered by each node for its own instances.
      </p>
      {panel.panel === "create" && <CreateNetworkPanel onClose={panel.close} />}
      {panel.panel === "delete" && panel.subject && (
        <DeleteConfirmation scope={projectScope("networks", project)} name={panel.subject} noun="network" onClose={panel.close} />
      )}
    </>
  );
}

function ExternalNetworks({ query }: { query: string }) {
  const project = useProject();
  const externalNetworks = useCollection<ExternalNetwork>({ plural: "external-networks" });
  const floatingIps = useCollection<FloatingIp>({ plural: "floating-ips", project });
  const panel = usePanel();
  const shown = externalNetworks.items.filter((network) => matches(query, network.metadata.name, network.specification.subnet));
  return (
    <>
      <CollectionNotice state={externalNetworks.state} noun="external networks" />
      <MainTable
        responsive
        emptyStateMsg="No external networks declared."
        headers={[
          { content: "Name" },
          { content: "Type" },
          { content: "Subnet" },
          { content: "Floating IP range" },
          { content: "Floating IPs in this project", className: "u-align--right" },
          { content: "Actions", className: "u-align--right" },
        ]}
        rows={shown.map((network) => ({
          key: network.metadata.name,
          columns: [
            { content: network.metadata.name },
            { content: <Tag emphasis="information">External</Tag> },
            { content: <span className="u-text--monospace">{network.specification.subnet}</span> },
            { content: <span className="u-text--monospace">{`${network.specification.first_address} – ${network.specification.last_address}`}</span> },
            {
              content: countOf(
                floatingIps.items.filter((floatingIp) => floatingIp.specification.external_network === network.metadata.name).length,
                "address",
                "addresses",
              ),
              className: "u-align--right",
            },
            {
              content: <IconAction icon="delete" label={`Delete ${network.metadata.name}`} onClick={() => panel.open("delete", network.metadata.name)} />,
              className: "u-align--right",
            },
          ],
        }))}
      />
      {panel.panel === "create" && <CreateExternalNetworkPanel onClose={panel.close} />}
      {panel.panel === "delete" && panel.subject && (
        <DeleteConfirmation scope={clusterScope("external-networks")} name={panel.subject} noun="external network" onClose={panel.close} />
      )}
    </>
  );
}

export default function Networks() {
  const panel = usePanel();
  const [query, setQuery] = useState("");
  const [tab, setTab] = useState<"private" | "external">("private");
  return (
    <div className="page">
      <PageHeader title="Networks">
        <SearchHeader noun="networks" value={query} onChange={setQuery} />
        <Button appearance="positive" className="u-no-margin--bottom" onClick={() => panel.open("create")}>
          {tab === "private" ? "Create network" : "Declare external network"}
        </Button>
      </PageHeader>
      <Tabs
        links={[
          { label: "Private", active: tab === "private", onClick: () => setTab("private") },
          { label: "External", active: tab === "external", onClick: () => setTab("external") },
        ]}
      />
      {tab === "private" ? <PrivateNetworks query={query} /> : <ExternalNetworks query={query} />}
    </div>
  );
}
