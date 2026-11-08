import { Button, Input, MainTable, Notification, Select } from "@canonical/react-components";
import { useState } from "react";

import { create, update } from "../cluster/client";
import type { Direction, Protocol, Remote, SecurityGroup, SecurityGroupRule, Vm } from "../cluster/kinds";
import { useCollection } from "../cluster/live";
import { projectScope } from "../cluster/scopes";
import CollectionNotice from "../components/CollectionNotice";
import Command from "../components/Command";
import DeleteConfirmation from "../components/DeleteConfirmation";
import IconAction from "../components/IconAction";
import PageHeader from "../components/PageHeader";
import Panel, { SubmitFooter } from "../components/Panel";
import SubjectLink from "../components/SubjectLink";
import Tag from "../components/Tag";
import { usePanel } from "../components/panels";
import { useSubmission } from "../components/submission";
import { countOf, sentence } from "../format";
import { useProject } from "../session";
import { useVms } from "./instances";

const GROUP_PREFIX = "group:";

export function describeRemote(remote: Remote): string {
  if (remote.type === "any") {
    return "anywhere";
  }
  return remote.type === "addresses" ? remote.network : `${GROUP_PREFIX} ${remote.name}`;
}

function describePorts(rule: SecurityGroupRule): string {
  const protocol = rule.protocol === "any" ? "Any" : rule.protocol.toUpperCase();
  if (!rule.ports) {
    return protocol;
  }
  const { first, last } = rule.ports;
  return `${protocol} ${first === last ? first : `${first}-${last}`}`;
}

export function ruleArgument(rule: SecurityGroupRule): string {
  const ports = rule.ports ? (rule.ports.first === rule.ports.last ? `${rule.ports.first}` : `${rule.ports.first}-${rule.ports.last}`) : "";
  const remote = rule.remote.type === "any" ? "any" : rule.remote.type === "addresses" ? rule.remote.network : `${GROUP_PREFIX}${rule.remote.name}`;
  return `${rule.direction},${rule.protocol},${ports},${remote}`;
}

function parsePorts(text: string): SecurityGroupRule["ports"] {
  if (!text.trim()) {
    return undefined;
  }
  const [first, last] = text.split("-");
  return { first: Number(first), last: Number(last ?? first) };
}

function parseRemote(text: string): Remote {
  const value = text.trim();
  if (value === "" || value === "any") {
    return { type: "any" };
  }
  if (value.startsWith(GROUP_PREFIX)) {
    return { type: "security-group", name: value.slice(GROUP_PREFIX.length) };
  }
  return { type: "addresses", network: value };
}

function usersOf(vms: Vm[], group: string): string[] {
  return vms
    .filter((vm) => (vm.specification.network_interfaces ?? []).some((nic) => (nic.security_groups ?? []).includes(group)))
    .map((vm) => vm.metadata.name);
}

function RuleEditor({ onAdd }: { onAdd: (rule: SecurityGroupRule) => void }) {
  const [direction, setDirection] = useState<Direction>("ingress");
  const [protocol, setProtocol] = useState<Protocol>("tcp");
  const [ports, setPorts] = useState("");
  const [remote, setRemote] = useState("any");
  const add = () => {
    onAdd({ direction, protocol, ports: parsePorts(ports), remote: parseRemote(remote) });
    setPorts("");
  };
  return (
    <div className="rule-editor">
      <Select
        id="rule-direction"
        label="Direction"
        value={direction}
        onChange={(event) => setDirection(event.target.value as Direction)}
        options={[
          { value: "ingress", label: "Inbound" },
          { value: "egress", label: "Outbound" },
        ]}
      />
      <Select
        id="rule-protocol"
        label="Protocol"
        value={protocol}
        onChange={(event) => setProtocol(event.target.value as Protocol)}
        options={["tcp", "udp", "icmp", "any"].map((choice) => ({ value: choice, label: choice.toUpperCase() }))}
      />
      <Input type="text" id="rule-ports" label="Ports" placeholder="443 or 8000-8100" value={ports} onChange={(event) => setPorts(event.target.value)} />
      <Input type="text"
        id="rule-remote"
        label="Source or destination"
        help="any, a CIDR, or group:NAME"
        value={remote}
        onChange={(event) => setRemote(event.target.value)}
      />
      <Button type="button" className="rule-editor__add" onClick={add}>
        Add rule
      </Button>
    </div>
  );
}

function RulesTable({ rules, onRemove }: { rules: SecurityGroupRule[]; onRemove?: (index: number) => void }) {
  return (
    <MainTable
      className="rules-table"
      emptyStateMsg="No rules: all inbound traffic is denied."
      headers={[{ content: "Direction" }, { content: "Port" }, { content: "Source / destination" }, { content: "" }]}
      rows={rules.map((rule, index) => ({
        key: `${ruleArgument(rule)}-${index}`,
        columns: [
          { content: rule.direction === "ingress" ? "Inbound" : "Outbound" },
          { content: <span className="u-text--monospace">{describePorts(rule)}</span> },
          { content: describeRemote(rule.remote) },
          {
            content: onRemove ? <IconAction icon="delete" label={`Remove rule ${ruleArgument(rule)}`} onClick={() => onRemove(index)} /> : null,
            className: "u-align--right",
          },
        ],
      }))}
    />
  );
}

function CreateGroupPanel({ onClose }: { onClose: () => void }) {
  const project = useProject();
  const [name, setName] = useState("");
  const [rules, setRules] = useState<SecurityGroupRule[]>([]);
  const { isBusy, problem, submit } = useSubmission();
  return (
    <Panel
      isWide
      title="Create security group"
      onClose={onClose}
      problem={problem}
      onSubmit={() => void submit(() => create(projectScope("security-groups", project), name, { rules }), onClose)}
      footer={<SubmitFooter label="Create" isBusy={isBusy} onCancel={onClose} />}
    >
      <Input type="text" id="group-name" label="Name" required value={name} onChange={(event) => setName(event.target.value)} />
      <h3 className="p-heading--5">Rules</h3>
      <RulesTable rules={rules} onRemove={(index) => setRules(rules.filter((_, position) => position !== index))} />
      <RuleEditor onAdd={(rule) => setRules([...rules, rule])} />
      <Command words={["amafu", "security-group", "create", name || "NAME", ...rules.flatMap((rule) => ["--rule", ruleArgument(rule)])]} />
    </Panel>
  );
}

function GroupDetail({ group, users, onClose }: { group: SecurityGroup; users: string[]; onClose: () => void }) {
  const project = useProject();
  const { problem, submit } = useSubmission();
  const change = (rules: SecurityGroupRule[]) =>
    void submit(() => update(projectScope("security-groups", project), group.metadata.name, { rules }, group.metadata.generation));
  const rules = group.specification.rules;
  return (
    <Panel title={group.metadata.name} onClose={onClose} problem={problem} isWide>
      <h3 className="p-heading--5">Rules</h3>
      <RulesTable rules={rules} onRemove={(index) => change(rules.filter((_, position) => position !== index))} />
      <RuleEditor onAdd={(rule) => change([...rules, rule])} />
      <h3 className="p-heading--5">Used by {countOf(users.length, "instance")}</h3>
      <div className="tag-list">
        {users.map((user) => (
          <Tag key={user}>{user}</Tag>
        ))}
      </div>
      <Command words={["amafu", "security-group", "show", group.metadata.name]} />
    </Panel>
  );
}

export default function SecurityGroups() {
  const project = useProject();
  const groups = useCollection<SecurityGroup>({ plural: "security-groups", project });
  const vms = useVms();
  const panel = usePanel();
  const subject = groups.items.find((group) => group.metadata.name === panel.subject);

  return (
    <div className="page">
      <PageHeader title="Security groups">
        <Button appearance="positive" className="u-no-margin--bottom" onClick={() => panel.open("create")}>
          Create security group
        </Button>
      </PageHeader>
      <CollectionNotice state={groups.state} noun="security groups" />
      {groups.state === "live" && groups.items.length === 0 && (
        <Notification severity="information" title="No security groups">
          Interfaces without a security group accept no inbound traffic.
        </Notification>
      )}
      <MainTable
        sortable
        responsive
        headers={[
          { content: "Name", sortKey: "name" },
          { content: "Rules" },
          { content: "Used by", sortKey: "users", className: "u-align--right" },
          { content: "Actions", className: "u-align--right" },
        ]}
        rows={groups.items.map((group) => {
          const users = usersOf(vms.items, group.metadata.name);
          return {
            key: group.metadata.name,
            columns: [
              { content: <SubjectLink name={group.metadata.name} /> },
              {
                content:
                  group.specification.rules
                    .map((rule) => `${sentence(rule.direction === "ingress" ? "in" : "out")} ${describePorts(rule)} ${describeRemote(rule.remote)}`)
                    .join("; ") || "No rules",
              },
              { content: countOf(users.length, "instance"), className: "u-align--right" },
              {
                content: <IconAction icon="delete" label={`Delete ${group.metadata.name}`} onClick={() => panel.open("delete", group.metadata.name)} />,
                className: "u-align--right",
              },
            ],
            sortData: { name: group.metadata.name, users: users.length },
          };
        })}
      />
      <p className="u-text--muted footnote">
        Rules are stateful and enforced on the node hosting each instance, so they move with it during live migration.
        Inbound traffic is denied unless a rule allows it.
      </p>
      {panel.panel === "create" && <CreateGroupPanel onClose={panel.close} />}
      {panel.panel === "detail" && subject && (
        <GroupDetail group={subject} users={usersOf(vms.items, subject.metadata.name)} onClose={panel.close} />
      )}
      {panel.panel === "delete" && panel.subject && (
        <DeleteConfirmation scope={projectScope("security-groups", project)} name={panel.subject} noun="security group" onClose={panel.close} />
      )}
    </div>
  );
}
