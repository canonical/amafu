import { Button, Input, MainTable, Notification, Select, Tabs, Textarea } from "@canonical/react-components";
import { useState } from "react";

import { create } from "../cluster/client";
import type { StorageBackend, StorageProvider, Volume } from "../cluster/kinds";
import { useCollection } from "../cluster/live";
import { clusterScope } from "../cluster/scopes";
import CollectionNotice from "../components/CollectionNotice";
import Command from "../components/Command";
import DeleteConfirmation from "../components/DeleteConfirmation";
import IconAction from "../components/IconAction";
import PageHeader from "../components/PageHeader";
import Panel, { SubmitFooter } from "../components/Panel";
import Status from "../components/Status";
import Tag from "../components/Tag";
import { usePanel } from "../components/panels";
import { useSubmission } from "../components/submission";
import { countOf, gibibytes, shortDigest } from "../format";
import { useProject } from "../session";

const PROVIDER_NAMES: Record<StorageProvider["type"], string> = {
  "local-disk": "Local disk",
  nfs: "NFS",
  "nvme-tcp": "NVMe/TCP",
  "ceph-rbd": "Ceph RBD",
  plugin: "Plugin",
};

function locationOf(provider: StorageProvider): string {
  switch (provider.type) {
    case "local-disk":
      return "the node running each VM";
    case "nfs":
      return `${provider.server}:${provider.export}`;
    case "nvme-tcp":
      return `${provider.management} · pool ${provider.pool}`;
    case "ceph-rbd":
      return `${provider.monitors.join(", ")} · pool ${provider.pool}`;
    case "plugin":
      return (provider.endpoints ?? []).join(", ") || "no endpoints";
  }
}

function pluginNameOf(url: string): string {
  return url.split("/").pop() ?? url;
}

function lines(text: string): string[] {
  return text
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);
}

function pairs(text: string): Record<string, string> {
  return Object.fromEntries(
    lines(text)
      .filter((line) => line.includes("="))
      .map((line) => [line.slice(0, line.indexOf("=")), line.slice(line.indexOf("=") + 1)]),
  );
}

interface ProviderFields {
  server: string;
  exportPath: string;
  management: string;
  managementUser: string;
  managementPassword: string;
  addresses: string;
  pool: string;
  monitors: string;
  cephUser: string;
  cephKey: string;
  commandEndpoint: string;
  commandUser: string;
  commandKey: string;
  moduleUrl: string;
  moduleDigest: string;
  configuration: string;
  endpoints: string;
  secrets: string;
}

const EMPTY_FIELDS: ProviderFields = {
  server: "",
  exportPath: "",
  management: "",
  managementUser: "",
  managementPassword: "",
  addresses: "",
  pool: "",
  monitors: "",
  cephUser: "",
  cephKey: "",
  commandEndpoint: "",
  commandUser: "",
  commandKey: "",
  moduleUrl: "",
  moduleDigest: "",
  configuration: "{}",
  endpoints: "",
  secrets: "",
};

function providerOf(type: StorageProvider["type"], fields: ProviderFields): unknown {
  switch (type) {
    case "local-disk":
      return { type };
    case "nfs":
      return { type, server: fields.server, export: fields.exportPath };
    case "nvme-tcp":
      return {
        type,
        management: fields.management,
        ...(fields.managementUser ? { management_user: fields.managementUser, management_password: fields.managementPassword } : {}),
        addresses: lines(fields.addresses),
        pool: fields.pool,
      };
    case "ceph-rbd":
      return {
        type,
        monitors: lines(fields.monitors),
        pool: fields.pool,
        user: fields.cephUser,
        key: fields.cephKey,
        management: fields.management,
        management_user: fields.managementUser,
        management_password: fields.managementPassword,
        command_endpoint: fields.commandEndpoint,
        command_user: fields.commandUser,
        command_key: fields.commandKey,
      };
    case "plugin": {
      let configuration: unknown = {};
      try {
        configuration = JSON.parse(fields.configuration || "{}");
      } catch {
        configuration = fields.configuration;
      }
      return {
        type,
        module: { url: fields.moduleUrl, digest: fields.moduleDigest },
        configuration,
        endpoints: lines(fields.endpoints),
        secrets: pairs(fields.secrets),
      };
    }
  }
}

function AddBackendPanel({ onClose, initialType }: { onClose: () => void; initialType: StorageProvider["type"] }) {
  const [name, setName] = useState("");
  const [type, setType] = useState<StorageProvider["type"]>(initialType);
  const [fields, setFields] = useState<ProviderFields>(EMPTY_FIELDS);
  const { isBusy, problem, submit } = useSubmission();
  const field = (key: keyof ProviderFields, label: string, extra: { help?: string; type?: string; multiline?: boolean } = {}) =>
    extra.multiline ? (
      <Textarea
        id={`backend-${key}`}
        label={label}
        help={extra.help}
        rows={3}
        value={fields[key]}
        onChange={(event) => setFields({ ...fields, [key]: event.target.value })}
      />
    ) : (
      <Input
        id={`backend-${key}`}
        label={label}
        help={extra.help}
        type={extra.type ?? "text"}
        autoComplete="off"
        value={fields[key]}
        onChange={(event) => setFields({ ...fields, [key]: event.target.value })}
      />
    );
  const commandWords =
    type === "local-disk"
      ? ["--local-disk"]
      : type === "nfs"
        ? ["--nfs", `${fields.server || "SERVER"}:${fields.exportPath || "EXPORT"}`]
        : ["--provider-file", "provider.yaml"];

  return (
    <Panel
      title="Add storage backend"
      onClose={onClose}
      problem={problem}
      onSubmit={() => void submit(() => create(clusterScope("storage-backends"), name, { provider: providerOf(type, fields) }), onClose)}
      footer={<SubmitFooter label="Add" isBusy={isBusy} onCancel={onClose} />}
    >
      <Input type="text" id="backend-name" label="Name" required value={name} onChange={(event) => setName(event.target.value)} />
      <Select
        id="backend-type"
        label="Provider"
        value={type}
        onChange={(event) => setType(event.target.value as StorageProvider["type"])}
        options={Object.entries(PROVIDER_NAMES).map(([value, label]) => ({ value, label }))}
      />
      {type === "local-disk" && <p className="u-text--muted">Volumes live on the disk of the node that runs the VM, so the VM cannot move.</p>}
      {type === "nfs" && (
        <>
          {field("server", "Server")}
          {field("exportPath", "Export")}
        </>
      )}
      {type === "nvme-tcp" && (
        <>
          {field("management", "Management endpoint", { help: "The SPDK JSON-RPC address." })}
          {field("managementUser", "Management user")}
          {field("managementPassword", "Management password", { type: "password" })}
          {field("addresses", "Target addresses", { help: "One ADDRESS:PORT per line.", multiline: true })}
          {field("pool", "Pool")}
        </>
      )}
      {type === "ceph-rbd" && (
        <>
          {field("monitors", "Monitors", { help: "One ADDRESS:PORT per line.", multiline: true })}
          {field("pool", "Pool")}
          {field("cephUser", "Client user")}
          {field("cephKey", "Client key", { type: "password" })}
          {field("management", "Dashboard API endpoint")}
          {field("managementUser", "Dashboard API user")}
          {field("managementPassword", "Dashboard API password", { type: "password" })}
          {field("commandEndpoint", "Command endpoint")}
          {field("commandUser", "Command user")}
          {field("commandKey", "Command key", { type: "password" })}
        </>
      )}
      {type === "plugin" && (
        <>
          {field("moduleUrl", "Module URL", { help: "Where the nodes fetch the WebAssembly component." })}
          {field("moduleDigest", "Module SHA-256", { help: "The module is refused unless its digest matches." })}
          {field("configuration", "Configuration", { help: "JSON handed to the plugin on every call.", multiline: true })}
          {field("endpoints", "Endpoints", { help: "One URL prefix per line; the plugin may reach nothing else.", multiline: true })}
          {field("secrets", "Secrets", { help: "One NAME=VALUE per line, readable only by this backend's plugin.", multiline: true })}
        </>
      )}
      <Command words={["amafu", "storage-backend", "create", name || "NAME", ...commandWords]} />
    </Panel>
  );
}

function Backends({ backends, volumes }: { backends: StorageBackend[]; volumes: Volume[] }) {
  const panel = usePanel();
  return (
    <>
      {backends
        .filter((backend) => backend.status.phase === "unreachable")
        .map((backend) => (
          <Notification key={backend.metadata.name} severity="caution" title={`${backend.metadata.name} is unreachable`}>
            {backend.status.message ?? "Volumes on it cannot be created, attached or migrated until it is back."}
          </Notification>
        ))}
      <MainTable
        sortable
        responsive
        emptyStateMsg="No storage backends declared."
        headers={[
          { content: "Name", sortKey: "name" },
          { content: "Provider", sortKey: "provider" },
          { content: "Location" },
          { content: "Volumes in this project", className: "u-align--right" },
          { content: "Provisioned", className: "u-align--right" },
          { content: "Status", sortKey: "status" },
          { content: "Actions", className: "u-align--right" },
        ]}
        rows={backends.map((backend) => {
          const provider = backend.specification.provider;
          const onBackend = volumes.filter((volume) => volume.specification.backend === backend.metadata.name);
          const provisioned = onBackend.reduce((total, volume) => total + volume.specification.size_gibibytes, 0);
          return {
            key: backend.metadata.name,
            columns: [
              { content: backend.metadata.name },
              {
                content:
                  provider.type === "plugin" ? (
                    <>
                      <Tag emphasis="information">Plugin</Tag> {pluginNameOf(provider.module.url)}
                    </>
                  ) : (
                    PROVIDER_NAMES[provider.type]
                  ),
              },
              { content: <span className="location-cell">{locationOf(provider)}</span> },
              { content: onBackend.length, className: "u-align--right" },
              { content: gibibytes(provisioned), className: "u-align--right" },
              { content: <Status phase={backend.status.phase ?? "unknown"} /> },
              {
                content: <IconAction icon="delete" label={`Delete ${backend.metadata.name}`} onClick={() => panel.open("delete", backend.metadata.name)} />,
                className: "u-align--right",
              },
            ],
            sortData: { name: backend.metadata.name, provider: provider.type, status: backend.status.phase },
          };
        })}
      />
    </>
  );
}

function Plugins({ backends }: { backends: StorageBackend[] }) {
  const modules = new Map<string, { url: string; digest: string; users: string[] }>();
  for (const backend of backends) {
    const provider = backend.specification.provider;
    if (provider.type === "plugin") {
      const key = `${provider.module.url}@${provider.module.digest}`;
      const known = modules.get(key) ?? { ...provider.module, users: [] };
      known.users.push(backend.metadata.name);
      modules.set(key, known);
    }
  }
  return (
    <>
      <p className="u-text--muted">
        Plugins are WebAssembly components that implement the storage provider contract. Each runs in a sandbox with its own
        memory, fuel and time limits, and can reach only the endpoints its backend declares.
      </p>
      <MainTable
        responsive
        emptyStateMsg="No plugin backends. Add one with the Plugin provider."
        headers={[{ content: "Module" }, { content: "Digest" }, { content: "Used by" }]}
        rows={[...modules.values()].map((module) => ({
          key: `${module.url}@${module.digest}`,
          columns: [
            { content: <span className="u-text--monospace">{module.url}</span> },
            { content: <span className="u-text--monospace">{shortDigest(module.digest)}</span> },
            { content: countOf(module.users.length, "backend") + `: ${module.users.join(", ")}` },
          ],
        }))}
      />
    </>
  );
}

export default function Storage() {
  const project = useProject();
  const backends = useCollection<StorageBackend>({ plural: "storage-backends" });
  const volumes = useCollection<Volume>({ plural: "volumes", project });
  const panel = usePanel();
  const [tab, setTab] = useState<"backends" | "plugins">("backends");
  return (
    <div className="page">
      <PageHeader title="Storage">
        <Button className="u-no-margin--bottom" onClick={() => panel.open("create", "plugin")}>
          Install plugin
        </Button>
        <Button appearance="positive" className="u-no-margin--bottom" onClick={() => panel.open("create")}>
          Add backend
        </Button>
      </PageHeader>
      <Tabs
        links={[
          { label: "Backends", active: tab === "backends", onClick: () => setTab("backends") },
          { label: "Plugins", active: tab === "plugins", onClick: () => setTab("plugins") },
        ]}
      />
      <CollectionNotice state={backends.state} noun="storage backends" />
      {tab === "backends" ? <Backends backends={backends.items} volumes={volumes.items} /> : <Plugins backends={backends.items} />}
      {panel.panel === "create" && (
        <AddBackendPanel onClose={panel.close} initialType={panel.subject === "plugin" ? "plugin" : "nvme-tcp"} />
      )}
      {panel.panel === "delete" && panel.subject && (
        <DeleteConfirmation
          scope={clusterScope("storage-backends")}
          name={panel.subject}
          noun="storage backend"
          consequence="Volumes already on it are not removed, but new ones cannot be created there."
          onClose={panel.close}
        />
      )}
    </div>
  );
}
