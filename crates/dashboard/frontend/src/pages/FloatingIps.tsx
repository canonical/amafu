import { Button, Card, Input, MainTable, Select } from "@canonical/react-components";
import { useState } from "react";

import { create, update } from "../cluster/client";
import type { ExternalNetwork, FloatingIp, Vm } from "../cluster/kinds";
import { isContainer } from "../cluster/kinds";
import { useCollection } from "../cluster/live";
import { projectScope } from "../cluster/scopes";
import CollectionNotice from "../components/CollectionNotice";
import Command from "../components/Command";
import DeleteConfirmation from "../components/DeleteConfirmation";
import IconAction from "../components/IconAction";
import Meter from "../components/Meter";
import PageHeader from "../components/PageHeader";
import Panel, { SubmitFooter } from "../components/Panel";
import Status from "../components/Status";
import SubjectLink from "../components/SubjectLink";
import { usePanel } from "../components/panels";
import { useSubmission } from "../components/submission";
import { useProject } from "../session";
import { useVms } from "./instances";

function addressValue(address: string): number {
  return address.split(".").reduce((value, octet) => value * 256 + Number(octet), 0);
}

function rangeSize(network: ExternalNetwork): number {
  return addressValue(network.specification.last_address) - addressValue(network.specification.first_address) + 1;
}

function addressOf(floatingIp: FloatingIp): string | undefined {
  return floatingIp.status.address ?? floatingIp.specification.address;
}

function TargetChoice({
  vms,
  vm,
  onVm,
  interfaceIndex,
  onInterface,
}: {
  vms: Vm[];
  vm: string;
  onVm: (vm: string) => void;
  interfaceIndex: number;
  onInterface: (index: number) => void;
}) {
  return (
    <div className="field-pair">
      <Select
        id="floating-target"
        label="Attach to"
        value={vm}
        onChange={(event) => onVm(event.target.value)}
        options={[
          { value: "", label: "Nothing for now" },
          ...vms.map((candidate) => ({
            value: candidate.metadata.name,
            label: isContainer(candidate) ? `${candidate.metadata.name} (container)` : candidate.metadata.name,
          })),
        ]}
      />
      <Input
        id="floating-interface"
        label="Interface"
        type="number"
        min={0}
        value={interfaceIndex}
        disabled={!vm}
        onChange={(event) => onInterface(Number(event.target.value))}
      />
    </div>
  );
}

function AllocatePanel({ externalNetworks, onClose }: { externalNetworks: ExternalNetwork[]; onClose: () => void }) {
  const project = useProject();
  const vms = useVms();
  const [name, setName] = useState("");
  const [network, setNetwork] = useState("");
  const [address, setAddress] = useState("");
  const [vm, setVm] = useState("");
  const [interfaceIndex, setInterfaceIndex] = useState(0);
  const { isBusy, problem, submit } = useSubmission();
  const chosenNetwork = network || externalNetworks[0]?.metadata.name || "";
  const specification = {
    external_network: chosenNetwork,
    ...(address ? { address } : {}),
    ...(vm ? { target: { vm, interface: interfaceIndex } } : {}),
  };
  return (
    <Panel
      title="Allocate floating IP"
      onClose={onClose}
      problem={problem}
      onSubmit={() => void submit(() => create(projectScope("floating-ips", project), name, specification), onClose)}
      footer={<SubmitFooter label="Allocate" isBusy={isBusy} onCancel={onClose} />}
    >
      <Input type="text" id="floating-name" label="Name" required value={name} onChange={(event) => setName(event.target.value)} />
      <Select
        id="floating-network"
        label="External network"
        value={chosenNetwork}
        onChange={(event) => setNetwork(event.target.value)}
        options={externalNetworks.map((candidate) => ({
          value: candidate.metadata.name,
          label: `${candidate.metadata.name} · ${candidate.specification.subnet}`,
        }))}
      />
      <Input type="text" id="floating-address" label="Address" help="Leave empty for the next free one." value={address} onChange={(event) => setAddress(event.target.value)} />
      <TargetChoice vms={vms.items} vm={vm} onVm={setVm} interfaceIndex={interfaceIndex} onInterface={setInterfaceIndex} />
      <Command
        words={[
          "amafu",
          "floating-ip",
          "create",
          name || "NAME",
          "--external-network",
          chosenNetwork || "NETWORK",
          address && "--address",
          address,
          vm && "--vm",
          vm,
          vm && interfaceIndex !== 0 && "--interface",
          vm && interfaceIndex !== 0 && String(interfaceIndex),
        ]}
      />
    </Panel>
  );
}

function MovePanel({ floatingIp, onClose }: { floatingIp: FloatingIp; onClose: () => void }) {
  const project = useProject();
  const vms = useVms();
  const [vm, setVm] = useState(floatingIp.specification.target?.vm ?? "");
  const [interfaceIndex, setInterfaceIndex] = useState(floatingIp.specification.target?.interface ?? 0);
  const { isBusy, problem, submit } = useSubmission();
  const { target: _previous, ...rest } = floatingIp.specification;
  const specification = vm ? { ...rest, target: { vm, interface: interfaceIndex } } : rest;
  return (
    <Panel
      title={`Move ${addressOf(floatingIp) ?? floatingIp.metadata.name}`}
      onClose={onClose}
      problem={problem}
      onSubmit={() =>
        void submit(
          () => update(projectScope("floating-ips", project), floatingIp.metadata.name, specification, floatingIp.metadata.generation),
          onClose,
        )
      }
      footer={<SubmitFooter label="Move" isBusy={isBusy} onCancel={onClose} />}
    >
      <TargetChoice vms={vms.items} vm={vm} onVm={setVm} interfaceIndex={interfaceIndex} onInterface={setInterfaceIndex} />
      <Command
        words={[
          "amafu",
          "floating-ip",
          "move",
          floatingIp.metadata.name,
          vm && "--vm",
          vm,
          vm && interfaceIndex !== 0 && "--interface",
          vm && interfaceIndex !== 0 && String(interfaceIndex),
        ]}
      />
    </Panel>
  );
}

function targetLink(floatingIp: FloatingIp, vms: Vm[]) {
  const target = floatingIp.specification.target;
  if (!target) {
    return <Status phase="unattached" />;
  }
  const vm = vms.find((candidate) => candidate.metadata.name === target.vm);
  const isContainerTarget = vm ? isContainer(vm) : false;
  return (
    <>
      <SubjectLink name={target.vm} page={isContainerTarget ? "/containers" : "/virtual-machines"} />
      {isContainerTarget && <span className="u-text--muted"> (container)</span>}
    </>
  );
}

export default function FloatingIps() {
  const project = useProject();
  const floatingIps = useCollection<FloatingIp>({ plural: "floating-ips", project });
  const externalNetworks = useCollection<ExternalNetwork>({ plural: "external-networks" });
  const vms = useVms();
  const panel = usePanel();
  const subject = floatingIps.items.find((floatingIp) => floatingIp.metadata.name === panel.subject);

  return (
    <div className="page">
      <PageHeader title="Floating IPs">
        <Button appearance="positive" className="u-no-margin--bottom" onClick={() => panel.open("create")}>
          Allocate floating IP
        </Button>
      </PageHeader>
      <div className="summary-cards">
        {externalNetworks.items.map((network) => {
          const allocated = floatingIps.items.filter((floatingIp) => floatingIp.specification.external_network === network.metadata.name).length;
          return (
            <Card key={network.metadata.name} className="summary-card">
              <h2 className="p-text--small-caps">
                {network.metadata.name} · <span className="u-text--monospace">{network.specification.subnet}</span>
              </h2>
              <Meter used={allocated} total={rangeSize(network)} label={`${allocated} of ${rangeSize(network)} addresses allocated in this project`} />
            </Card>
          );
        })}
      </div>
      <CollectionNotice state={floatingIps.state} noun="floating IPs" />
      <MainTable
        sortable
        responsive
        emptyStateMsg="No floating IPs allocated."
        headers={[
          { content: "Address", sortKey: "address" },
          { content: "Name", sortKey: "name" },
          { content: "External network" },
          { content: "Attached to" },
          { content: "Node" },
          { content: "Status" },
          { content: "Actions", className: "u-align--right" },
        ]}
        rows={floatingIps.items.map((floatingIp) => ({
          key: floatingIp.metadata.name,
          columns: [
            { content: <span className="u-text--monospace">{addressOf(floatingIp) ?? "—"}</span> },
            { content: floatingIp.metadata.name },
            { content: floatingIp.specification.external_network },
            { content: targetLink(floatingIp, vms.items) },
            {
              content: floatingIp.status.binding ? (
                <>
                  {floatingIp.status.binding.node}
                  <div className="secondary-line">follows the instance</div>
                </>
              ) : (
                "—"
              ),
            },
            { content: <Status phase={floatingIp.status.phase ?? "pending"} detail={floatingIp.status.phase === "failed" ? floatingIp.status.message : undefined} /> },
            {
              content: (
                <div className="row-actions">
                  <IconAction icon="exposed" label={`Move ${floatingIp.metadata.name}`} onClick={() => panel.open("move", floatingIp.metadata.name)} />
                  <IconAction icon="delete" label={`Release ${floatingIp.metadata.name}`} onClick={() => panel.open("delete", floatingIp.metadata.name)} />
                </div>
              ),
              className: "u-align--right",
            },
          ],
          sortData: { address: addressValue(addressOf(floatingIp) ?? "0.0.0.0"), name: floatingIp.metadata.name },
        }))}
      />
      <p className="u-text--muted footnote">
        SNAT and floating IPs run on the node hosting the instance, so they move with it during live migration.
      </p>
      {panel.panel === "create" && <AllocatePanel externalNetworks={externalNetworks.items} onClose={panel.close} />}
      {panel.panel === "move" && subject && <MovePanel floatingIp={subject} onClose={panel.close} />}
      {panel.panel === "delete" && subject && (
        <DeleteConfirmation
          scope={projectScope("floating-ips", project)}
          name={subject.metadata.name}
          noun="floating IP"
          consequence="The address returns to its external network and may be given to someone else."
          onClose={panel.close}
        />
      )}
    </div>
  );
}
