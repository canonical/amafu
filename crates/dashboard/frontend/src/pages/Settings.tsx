import { Button, Input, MainTable, Select } from "@canonical/react-components";
import { useState } from "react";

import { create } from "../cluster/client";
import type { AuditSink, AuditSinkSpecification, ClusterVersion } from "../cluster/kinds";
import { useCollection } from "../cluster/live";
import { clusterScope } from "../cluster/scopes";
import CollectionNotice from "../components/CollectionNotice";
import Command from "../components/Command";
import DeleteConfirmation from "../components/DeleteConfirmation";
import IconAction from "../components/IconAction";
import PageHeader from "../components/PageHeader";
import Panel, { SubmitFooter } from "../components/Panel";
import Status from "../components/Status";
import { usePanel } from "../components/panels";
import { useSubmission } from "../components/submission";
import { shortDigest } from "../format";
import { DetailList } from "./instances";

const SINKS = clusterScope("audit-sinks");

function destinationOf(sink: AuditSinkSpecification): string {
  return sink.type === "syslog" ? sink.address : `${sink.endpoint}/${sink.bucket}/${sink.prefix}`;
}

function AddSinkPanel({ onClose }: { onClose: () => void }) {
  const [name, setName] = useState("");
  const [type, setType] = useState<AuditSinkSpecification["type"]>("syslog");
  const [address, setAddress] = useState("");
  const [serverName, setServerName] = useState("");
  const [endpoint, setEndpoint] = useState("");
  const [bucket, setBucket] = useState("");
  const [region, setRegion] = useState("");
  const [prefix, setPrefix] = useState("amafu-audit/");
  const [accessKey, setAccessKey] = useState("");
  const [secretKey, setSecretKey] = useState("");
  const { isBusy, problem, submit } = useSubmission();
  const specification: AuditSinkSpecification =
    type === "syslog"
      ? { type, address, ...(serverName ? { server_name: serverName } : {}) }
      : { type, endpoint, bucket, region, prefix, access_key: accessKey, secret_key: secretKey };
  return (
    <Panel
      title="Export the audit log"
      onClose={onClose}
      problem={problem}
      onSubmit={() => void submit(() => create(SINKS, name, specification), onClose)}
      footer={<SubmitFooter label="Start exporting" isBusy={isBusy} onCancel={onClose} />}
    >
      <p className="u-text--muted">Every audit entry is delivered at least once, in order, starting from the oldest kept.</p>
      <Input type="text" id="sink-name" label="Name" required value={name} onChange={(event) => setName(event.target.value)} />
      <Select
        id="sink-type"
        label="Destination"
        value={type}
        onChange={(event) => setType(event.target.value as AuditSinkSpecification["type"])}
        options={[
          { value: "syslog", label: "Syslog over TCP (RFC 5424)" },
          { value: "object-storage", label: "S3-compatible object storage" },
        ]}
      />
      {type === "syslog" ? (
        <>
          <Input type="text" id="sink-address" label="Address" placeholder="syslog.example:6514" required value={address} onChange={(event) => setAddress(event.target.value)} />
          <Input type="text" id="sink-server-name" label="TLS server name" help="Leave empty for plain TCP." value={serverName} onChange={(event) => setServerName(event.target.value)} />
        </>
      ) : (
        <>
          <Input type="text" id="sink-endpoint" label="Endpoint" required value={endpoint} onChange={(event) => setEndpoint(event.target.value)} />
          <div className="field-pair">
            <Input type="text" id="sink-bucket" label="Bucket" required value={bucket} onChange={(event) => setBucket(event.target.value)} />
            <Input type="text" id="sink-region" label="Region" required value={region} onChange={(event) => setRegion(event.target.value)} />
          </div>
          <Input type="text" id="sink-prefix" label="Prefix" value={prefix} onChange={(event) => setPrefix(event.target.value)} />
          <Input type="text" id="sink-access-key" label="Access key" autoComplete="off" required value={accessKey} onChange={(event) => setAccessKey(event.target.value)} />
          <Input id="sink-secret-key" label="Secret key" type="password" autoComplete="off" required value={secretKey} onChange={(event) => setSecretKey(event.target.value)} />
        </>
      )}
      <Command words={["amafu", "audit-sink", "create", name || "NAME", "--file", "sink.yaml"]} />
    </Panel>
  );
}

export default function Settings() {
  const versions = useCollection<ClusterVersion>({ plural: "cluster-versions" });
  const sinks = useCollection<AuditSink>(SINKS);
  const panel = usePanel();
  const version = versions.items[0];

  return (
    <div className="page">
      <PageHeader title="Settings" />
      <h2 className="p-heading--4">Cluster version</h2>
      <CollectionNotice state={versions.state} noun="cluster versions" />
      {version && (
        <DetailList
          rows={[
            { label: "Image", value: version.specification.image.version },
            { label: "Digest", value: shortDigest(version.specification.image.digest) },
            { label: "Feature level", value: version.specification.image.feature_level },
            { label: "Finalized level", value: version.specification.finalized_feature_level },
            { label: "Previous image", value: version.specification.previous_image?.version },
          ]}
        />
      )}
      <p className="u-text--muted">
        Upgrades are started from the command line, where the signed image is checked before any node writes it.
      </p>
      <Command words={["amafu", "upgrade", "start", "--image", "image-manifest.json"]} title="Upgrade" />
      <div className="section-header">
        <h2 className="p-heading--4 u-no-margin--bottom">Audit export</h2>
        <Button appearance="positive" className="u-no-margin--bottom" onClick={() => panel.open("create")}>
          Add audit sink
        </Button>
      </div>
      <CollectionNotice state={sinks.state} noun="audit sinks" />
      <MainTable
        responsive
        emptyStateMsg="The audit log is kept only in the cluster."
        headers={[
          { content: "Name" },
          { content: "Type" },
          { content: "Destination" },
          { content: "Next entry", className: "u-align--right" },
          { content: "Status" },
          { content: "Actions", className: "u-align--right" },
        ]}
        rows={sinks.items.map((sink) => ({
          key: sink.metadata.name,
          columns: [
            { content: sink.metadata.name },
            { content: sink.specification.type === "syslog" ? "Syslog" : "Object storage" },
            { content: <span className="u-text--monospace">{destinationOf(sink.specification)}</span> },
            { content: sink.status.next_sequence ?? 0, className: "u-align--right" },
            {
              content: sink.status.last_failure ? (
                <Status phase="failed" label="Retrying" detail={sink.status.last_failure} />
              ) : (
                <Status phase="running" label="Delivering" />
              ),
            },
            {
              content: <IconAction icon="delete" label={`Delete ${sink.metadata.name}`} onClick={() => panel.open("delete", sink.metadata.name)} />,
              className: "u-align--right",
            },
          ],
        }))}
      />
      {panel.panel === "create" && <AddSinkPanel onClose={panel.close} />}
      {panel.panel === "delete" && panel.subject && <DeleteConfirmation scope={SINKS} name={panel.subject} noun="audit sink" onClose={panel.close} />}
    </div>
  );
}
