import { Button, Chip, Input, MainTable, Notification, Textarea } from "@canonical/react-components";
import { useEffect, useState } from "react";

import { create, describeFailure, update } from "../cluster/client";
import type { DiscoveredKey, KeyLookup, KeySource, SshKey, SshKeyGroup, Vm } from "../cluster/kinds";
import { useCollection } from "../cluster/live";
import { projectScope } from "../cluster/scopes";
import {
  canGenerate,
  describeOrigin,
  download,
  fingerprint,
  generateKeyPair,
  importName,
  lookUpKeys,
  newKeys,
  parsePublicKey,
  publicKeyProblem,
} from "../cluster/sshKeys";
import ChipSelect from "../components/ChipSelect";
import CollectionNotice from "../components/CollectionNotice";
import Command from "../components/Command";
import DeleteConfirmation from "../components/DeleteConfirmation";
import IconAction from "../components/IconAction";
import PageHeader from "../components/PageHeader";
import Panel, { SubmitFooter } from "../components/Panel";
import PanelTabs from "../components/PanelTabs";
import Segments from "../components/Segments";
import SubjectLink from "../components/SubjectLink";
import Tag from "../components/Tag";
import { usePanel } from "../components/panels";
import { useSubmission } from "../components/submission";
import { countOf } from "../format";
import { useProject } from "../session";
import { nameProblem } from "./environmentForms";
import { DetailList, useVms } from "./instances";

type Method = "paste" | "generate" | KeySource;

const SOURCE_NAMES: Record<KeySource, string> = { github: "GitHub", launchpad: "Launchpad" };

function keyScope(project: string | null) {
  return projectScope("ssh-keys", project);
}

function groupScope(project: string | null) {
  return projectScope("ssh-key-groups", project);
}

function useFingerprint(publicKey: string): string {
  const [value, setValue] = useState("");
  useEffect(() => {
    let isCurrent = true;
    void fingerprint(publicKey).then((computed) => isCurrent && setValue(computed));
    return () => {
      isCurrent = false;
    };
  }, [publicKey]);
  return value;
}

const SHORT_FINGERPRINT = 19;

function ShortFingerprint({ publicKey }: { publicKey: string }) {
  const full = useFingerprint(publicKey);
  return (
    <span className="u-text--monospace fingerprint" title={full}>
      {full ? `${full.slice(0, SHORT_FINGERPRINT)}…` : "…"}
    </span>
  );
}

function grantedTo(vms: Vm[], publicKeyFingerprint: string): string[] {
  return vms
    .filter((vm) => (vm.status.authorized_keys ?? []).some((granted) => granted.fingerprint === publicKeyFingerprint))
    .map((vm) => vm.metadata.name);
}

function groupsHolding(groups: SshKeyGroup[], key: string): string[] {
  return groups.filter((group) => (group.specification.keys ?? []).includes(key)).map((group) => group.metadata.name);
}

function PastedKey({ publicKey, setPublicKey }: { publicKey: string; setPublicKey: (publicKey: string) => void }) {
  return (
    <Textarea
      id="ssh-key-public"
      label="Public key"
      rows={4}
      required
      placeholder="ssh-ed25519 AAAA… you@laptop"
      value={publicKey}
      error={publicKeyProblem(publicKey)}
      help="The contents of a .pub file, such as ~/.ssh/id_ed25519.pub."
      onChange={(event) => setPublicKey(event.target.value)}
    />
  );
}

function DiscoveredKeys({ lookup, existing, excluded, onExclude }: {
  lookup: KeyLookup;
  existing: SshKey[];
  excluded: string[];
  onExclude: (excluded: string[]) => void;
}) {
  const fresh = newKeys(lookup, existing);
  const known = lookup.keys.length - fresh.length;
  const label = (key: DiscoveredKey) => `${key.algorithm.replace("ssh-", "")} ${key.comment ?? key.fingerprint.slice(7, 19)}`;
  return (
    <div className="discovered-keys">
      <p className="p-form__label">
        {countOf(fresh.length, "new key")} published by {lookup.account}
        {known > 0 && `, ${known} already here`}
      </p>
      <div className="chip-select__chips">
        {fresh
          .filter((key) => !excluded.includes(key.fingerprint))
          .map((key) => (
            <Chip key={key.fingerprint} value={label(key)} onDismiss={() => onExclude([...excluded, key.fingerprint])} />
          ))}
      </div>
      {(lookup.skipped ?? []).map((reason) => (
        <p key={reason} className="p-form-help-text">Skipped a key: {reason}</p>
      ))}
    </div>
  );
}

function AddKeyPanel({ keys, groups, onClose }: { keys: SshKey[]; groups: SshKeyGroup[]; onClose: () => void }) {
  const project = useProject();
  const [method, setMethod] = useState<Method>("paste");
  const [name, setName] = useState("");
  const [publicKey, setPublicKey] = useState("");
  const [comment, setComment] = useState("");
  const [account, setAccount] = useState("");
  const [lookup, setLookup] = useState<KeyLookup | null>(null);
  const [lookupProblem, setLookupProblem] = useState<string | null>(null);
  const [excluded, setExcluded] = useState<string[]>([]);
  const [group, setGroup] = useState("");
  const { isBusy, problem, submit } = useSubmission();
  const taken = keys.map((key) => key.metadata.name);
  const isImport = method === "github" || method === "launchpad";

  const findKeys = async () => {
    setLookup(null);
    setLookupProblem(null);
    setExcluded([]);
    try {
      setLookup(await lookUpKeys(project, method as KeySource, account.trim()));
    } catch (failure) {
      setLookupProblem(describeFailure(failure));
    }
  };

  const importKeys = async () => {
    if (!lookup) {
      throw new Error("Look up the account's keys first");
    }
    const names = [...taken];
    const chosen = newKeys(lookup, keys).filter((key) => !excluded.includes(key.fingerprint));
    const created: string[] = [];
    for (const key of chosen) {
      const keyName = importName(lookup.source, lookup.account, names);
      names.push(keyName);
      await create(keyScope(project), keyName, { public_key: key.public_key, origin: { source: lookup.source, account: lookup.account } });
      created.push(keyName);
    }
    if (group) {
      const existing = groups.find((candidate) => candidate.metadata.name === group);
      const already = keys.filter((key) => lookup.keys.some((found) => parsePublicKey(found.public_key)?.blob === parsePublicKey(key.specification.public_key)?.blob));
      const members = [...created, ...already.map((key) => key.metadata.name)];
      if (existing) {
        const current = existing.specification.keys ?? [];
        await update(groupScope(project), group, { keys: [...current, ...members.filter((member) => !current.includes(member))] }, existing.metadata.generation);
      } else {
        await create(groupScope(project), group, { keys: members });
      }
    }
  };

  const generate = async () => {
    const pair = await generateKeyPair(comment.trim() || name);
    await create(keyScope(project), name, { public_key: pair.publicKey, origin: { source: "generated" } });
    download(name, pair.privateKey);
  };

  const add = () => {
    switch (method) {
      case "paste":
        return create(keyScope(project), name, { public_key: publicKey.trim(), origin: { source: "pasted" } });
      case "generate":
        return generate();
      default:
        return importKeys();
    }
  };

  const command = {
    paste: ["amafu", "ssh-key", "add", name || "NAME", "PUBLIC_KEY_OR_FILE"],
    generate: ["amafu", "ssh-key", "generate", name || "NAME", comment && "--comment", comment],
    github: ["amafu", "ssh-key", "import", `github:${account || "USER"}`, group && "--group", group],
    launchpad: ["amafu", "ssh-key", "import", `launchpad:${account || "USER"}`, group && "--group", group],
  }[method];

  return (
    <Panel
      isWide
      title="Add SSH key"
      onClose={onClose}
      problem={problem}
      onSubmit={() => void submit(add, onClose)}
      footer={<SubmitFooter label={method === "generate" ? "Generate and download" : isImport ? "Import" : "Add"} isBusy={isBusy} onCancel={onClose} />}
    >
      <Segments<Method>
        label="Key"
        value={method}
        onChange={setMethod}
        segments={[
          { value: "paste", label: "Paste" },
          { value: "generate", label: "Generate" },
          { value: "github", label: "GitHub" },
          { value: "launchpad", label: "Launchpad" },
        ]}
      />
      {!isImport && (
        <Input type="text" id="ssh-key-name" label="Name" required value={name} error={nameProblem(name, taken)} onChange={(event) => setName(event.target.value)} />
      )}
      {method === "paste" && <PastedKey publicKey={publicKey} setPublicKey={setPublicKey} />}
      {method === "generate" && (
        <>
          <Input type="text" id="ssh-key-comment" label="Comment" placeholder={name || "you@laptop"} value={comment} onChange={(event) => setComment(event.target.value)} />
          {canGenerate() ? (
            <Notification severity="caution" title="Save the private key">
              Your browser makes an Ed25519 key pair and downloads the private key. Amafu receives only the public key, so the private key cannot be downloaded again.
            </Notification>
          ) : (
            <Notification severity="negative" title="This browser cannot make keys">
              Use <code>amafu ssh-key generate</code> instead.
            </Notification>
          )}
        </>
      )}
      {isImport && (
        <>
          <div className="field-pair field-pair--action">
            <Input
              type="text"
              id="ssh-key-account"
              label={`${SOURCE_NAMES[method]} user`}
              required
              value={account}
              error={lookupProblem ?? undefined}
              onChange={(event) => setAccount(event.target.value)}
            />
            <Button type="button" disabled={!account.trim()} onClick={() => void findKeys()}>
              Look up keys
            </Button>
          </div>
          {lookup && <DiscoveredKeys lookup={lookup} existing={keys} excluded={excluded} onExclude={setExcluded} />}
          <Input
            type="text"
            id="ssh-key-import-group"
            label="Add to group"
            list="ssh-key-group-names"
            placeholder="Optional; a new or existing group"
            value={group}
            onChange={(event) => setGroup(event.target.value)}
          />
          <datalist id="ssh-key-group-names">
            {groups.map((candidate) => (
              <option key={candidate.metadata.name} value={candidate.metadata.name} />
            ))}
          </datalist>
          <p className="p-form-help-text">
            Keys are copied once. Import again later to pick up keys the account added; keys it removed stay here until you delete them.
          </p>
        </>
      )}
      <Command words={command} />
    </Panel>
  );
}

function KeyDetail({ sshKey, groups, vms, onClose }: { sshKey: SshKey; groups: SshKeyGroup[]; vms: Vm[]; onClose: () => void }) {
  const keyFingerprint = useFingerprint(sshKey.specification.public_key);
  const parsed = parsePublicKey(sshKey.specification.public_key);
  const holders = groupsHolding(groups, sshKey.metadata.name);
  const granted = grantedTo(vms, keyFingerprint);
  return (
    <Panel title={sshKey.metadata.name} onClose={onClose}>
      <DetailList
        rows={[
          { label: "Type", value: parsed?.algorithm ?? "—" },
          { label: "Fingerprint", value: <span className="u-text--monospace">{keyFingerprint || "…"}</span> },
          { label: "Comment", value: parsed?.comment ?? "—" },
          { label: "Origin", value: describeOrigin(sshKey.specification.origin) },
        ]}
      />
      <Textarea id="ssh-key-detail-public" label="Public key" readOnly rows={4} value={sshKey.specification.public_key} />
      <h3 className="p-heading--5">In {countOf(holders.length, "group")}</h3>
      <div className="tag-list">{holders.map((holder) => <Tag key={holder}>{holder}</Tag>)}</div>
      <h3 className="p-heading--5">Granted to {countOf(granted.length, "instance")}</h3>
      <div className="tag-list">{granted.map((vm) => <Tag key={vm}>{vm}</Tag>)}</div>
      <Command words={["amafu", "ssh-key", "show", sshKey.metadata.name]} />
    </Panel>
  );
}

function GroupPanel({ group, keys, vms, taken, onClose }: {
  group?: SshKeyGroup;
  keys: SshKey[];
  vms: Vm[];
  taken: string[];
  onClose: () => void;
}) {
  const project = useProject();
  const [name, setName] = useState(group?.metadata.name ?? "");
  const [members, setMembers] = useState<string[]>(group?.specification.keys ?? []);
  const { isBusy, problem, submit } = useSubmission();
  const save = () =>
    group
      ? update(groupScope(project), group.metadata.name, { keys: members }, group.metadata.generation)
      : create(groupScope(project), name, { keys: members });
  const users = group
    ? vms.filter((vm) => (vm.specification.ssh_keys ?? []).some((reference) => reference.kind === "group" && reference.name === group.metadata.name)).map((vm) => vm.metadata.name)
    : [];
  return (
    <Panel
      title={group ? group.metadata.name : "Create key group"}
      onClose={onClose}
      problem={problem}
      onSubmit={() => void submit(save, onClose)}
      footer={<SubmitFooter label={group ? "Save" : "Create"} isBusy={isBusy} onCancel={onClose} />}
    >
      {!group && (
        <Input type="text" id="ssh-key-group-name" label="Name" required value={name} error={nameProblem(name, taken)} onChange={(event) => setName(event.target.value)} />
      )}
      <ChipSelect
        id="ssh-key-group-keys"
        label="Keys"
        choices={keys.map((key) => ({ value: key.metadata.name, label: key.metadata.name, detail: describeOrigin(key.specification.origin) }))}
        selected={members}
        onChange={setMembers}
        placeholder="Search keys"
        emptyMessage="Add keys first"
        help="Instances created later with this group get these keys. Instances already running keep the keys they started with."
      />
      {group && (
        <>
          <h3 className="p-heading--5">Chosen by {countOf(users.length, "instance")}</h3>
          <div className="tag-list">{users.map((user) => <Tag key={user}>{user}</Tag>)}</div>
        </>
      )}
      <Command
        words={
          group
            ? ["amafu", "ssh-key-group", "add", group.metadata.name, ...members.filter((member) => !(group.specification.keys ?? []).includes(member))]
            : ["amafu", "ssh-key-group", "create", name || "NAME", ...members.flatMap((member) => ["--key", member])]
        }
      />
    </Panel>
  );
}

function KeysTable({ keys, groups, onDelete }: { keys: SshKey[]; groups: SshKeyGroup[]; onDelete: (name: string) => void }) {
  return (
    <MainTable
      sortable
      responsive
      emptyStateMsg="No SSH keys yet."
      headers={[
        { content: "Name", sortKey: "name" },
        { content: "Fingerprint" },
        { content: "Comment" },
        { content: "Origin", sortKey: "origin" },
        { content: "Groups" },
        { content: "Actions", className: "u-align--right" },
      ]}
      rows={keys.map((key) => ({
        key: key.metadata.name,
        columns: [
          { content: <SubjectLink name={key.metadata.name} panel="key" /> },
          { content: <ShortFingerprint publicKey={key.specification.public_key} /> },
          { content: parsePublicKey(key.specification.public_key)?.comment ?? "—" },
          { content: describeOrigin(key.specification.origin) },
          { content: groupsHolding(groups, key.metadata.name).join(", ") || "—" },
          { content: <IconAction icon="delete" label={`Delete ${key.metadata.name}`} onClick={() => onDelete(key.metadata.name)} />, className: "u-align--right" },
        ],
        sortData: { name: key.metadata.name, origin: describeOrigin(key.specification.origin) },
      }))}
    />
  );
}

function GroupsTable({ groups, onDelete }: { groups: SshKeyGroup[]; onDelete: (name: string) => void }) {
  return (
    <MainTable
      sortable
      responsive
      emptyStateMsg="No key groups yet. A group grants several keys at once."
      headers={[
        { content: "Name", sortKey: "name" },
        { content: "Keys" },
        { content: "Actions", className: "u-align--right" },
      ]}
      rows={groups.map((group) => ({
        key: group.metadata.name,
        columns: [
          { content: <SubjectLink name={group.metadata.name} panel="group" /> },
          { content: (group.specification.keys ?? []).join(", ") || "—" },
          { content: <IconAction icon="delete" label={`Delete ${group.metadata.name}`} onClick={() => onDelete(group.metadata.name)} />, className: "u-align--right" },
        ],
        sortData: { name: group.metadata.name },
      }))}
    />
  );
}

export default function SshKeys() {
  const project = useProject();
  const keys = useCollection<SshKey>({ plural: "ssh-keys", project });
  const groups = useCollection<SshKeyGroup>({ plural: "ssh-key-groups", project });
  const vms = useVms();
  const panel = usePanel();
  const sshKey = keys.items.find((candidate) => candidate.metadata.name === panel.subject);
  const group = groups.items.find((candidate) => candidate.metadata.name === panel.subject);
  return (
    <div className="page">
      <PageHeader title="SSH keys">
        <Button className="u-no-margin--bottom" onClick={() => panel.open("create-group")}>
          Create key group
        </Button>
        <Button appearance="positive" className="u-no-margin--bottom" onClick={() => panel.open("add")}>
          Add SSH key
        </Button>
      </PageHeader>
      <CollectionNotice state={keys.state} noun="SSH keys" />
      <PanelTabs
        tabs={[
          {
            label: `Keys (${keys.items.length})`,
            content: <KeysTable keys={keys.items} groups={groups.items} onDelete={(name) => panel.open("delete-key", name)} />,
          },
          { label: `Groups (${groups.items.length})`, content: <GroupsTable groups={groups.items} onDelete={(name) => panel.open("delete-group", name)} /> },
        ]}
      />
      <p className="u-text--muted footnote">
        Amafu keeps public keys only. An instance gets its keys when it is first placed, and keeps them: deleting or changing a key later does not reach instances that already started.
      </p>
      {panel.panel === "add" && <AddKeyPanel keys={keys.items} groups={groups.items} onClose={panel.close} />}
      {panel.panel === "key" && sshKey && <KeyDetail sshKey={sshKey} groups={groups.items} vms={vms.items} onClose={panel.close} />}
      {panel.panel === "create-group" && (
        <GroupPanel keys={keys.items} vms={vms.items} taken={groups.items.map((candidate) => candidate.metadata.name)} onClose={panel.close} />
      )}
      {panel.panel === "group" && group && <GroupPanel key={group.metadata.name} group={group} keys={keys.items} vms={vms.items} taken={[]} onClose={panel.close} />}
      {panel.panel === "delete-key" && panel.subject && (
        <DeleteConfirmation
          scope={keyScope(project)}
          name={panel.subject}
          noun="SSH key"
          consequence="Instances that already have this key keep it. Remove it from its groups first."
          onClose={panel.close}
        />
      )}
      {panel.panel === "delete-group" && panel.subject && (
        <DeleteConfirmation scope={groupScope(project)} name={panel.subject} noun="key group" consequence="The keys stay; instances that already have them keep them." onClose={panel.close} />
      )}
    </div>
  );
}
