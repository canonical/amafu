import { ConfirmationModal, Input, Notification, Select, Switch } from "@canonical/react-components";
import { useState } from "react";

import type { JujuController, JujuRequest, KubernetesCluster, MachineShape, Substrate } from "../cluster/environments";
import { MAXIMUM_PODS, isValidPodCount, requestJujuChange } from "../cluster/environments";
import Command from "../components/Command";
import Panel, { SubmitFooter } from "../components/Panel";
import SearchSelect from "../components/SearchSelect";
import Segments from "../components/Segments";
import Wizard from "../components/Wizard";
import { useSubmission } from "../components/submission";
import { useProject } from "../session";
import { NumberField, PlacementFields, ShapeSelect, chosen, isPlacementReady, nameProblem, usePlacementChoices } from "./environmentForms";

const VERSIONS = ["3.6.29", "4.0.16"];
const MAXIMUM_UNITS = 100;

export function CreateControllerPanel({
  taken,
  clusters,
  onClose,
}: {
  taken: string[];
  clusters: KubernetesCluster[];
  onClose: () => void;
}) {
  const project = useProject();
  const choices = usePlacementChoices();
  const readyClusters = clusters.filter((cluster) => cluster.status.phase === "ready").map((cluster) => cluster.metadata.name);
  const [name, setName] = useState("");
  const [version, setVersion] = useState(VERSIONS[0]);
  const [on, setOn] = useState<"pods" | "kubernetes">("pods");
  const [pods, setPods] = useState(1);
  const [isHighlyAvailable, setHighlyAvailable] = useState(false);
  const [shape, setShape] = useState<MachineShape>("medium");
  const [cluster, setCluster] = useState("");
  const [firstModel, setFirstModel] = useState("default");
  const [placement, setPlacement] = useState({ network: "", storageBackend: "", externalNetwork: "" });
  const { isBusy, problem, submit } = useSubmission();
  const where = chosen(placement, choices);
  const hostCluster = cluster || readyClusters[0] || "";
  const controllers = isHighlyAvailable ? 3 : 1;
  const podProblem = isValidPodCount(pods) ? (controllers > pods ? "High availability needs at least 3 pods." : undefined) : `Use 1 pod, or 3 to ${MAXIMUM_PODS} so the LXD cluster keeps a quorum.`;
  const substrate: Substrate =
    on === "pods"
      ? {
          on,
          pods,
          controllers,
          shape,
          network: where.network,
          storage_backend: where.storageBackend,
          external_network: where.externalNetwork,
        }
      : { on, cluster: hostCluster };
  const problemWithName = nameProblem(name, taken);
  const controller = { version, substrate, first_model: firstModel, sign_in: "amafu" as const };
  const isPlaced = on === "pods" ? isPlacementReady(where, choices) && !podProblem : Boolean(hostCluster);
  return (
    <Wizard
      title="Create Juju controller"
      submitLabel="Create controller"
      isBusy={isBusy}
      problem={problem}
      onClose={onClose}
      onSubmit={() => void submit(() => requestJujuChange(project, name, { kind: "create", controller }), onClose)}
      steps={[
        {
          title: "Controller",
          summary: `${name} · Juju ${version}`,
          isReady: Boolean(name) && !problemWithName,
          content: (
            <>
              <Input id="controller-name" type="text" label="Name" required value={name} error={problemWithName} onChange={(event) => setName(event.target.value)} />
              <Select id="controller-version" label="Juju version" value={version} onChange={(event) => setVersion(event.target.value)} options={VERSIONS.map((value) => ({ value, label: value }))} />
              <Segments<"pods" | "kubernetes">
                label="Runs on"
                value={on}
                onChange={setOn}
                segments={[
                  { value: "pods", label: "LXD pods" },
                  { value: "kubernetes", label: "A Kubernetes cluster" },
                ]}
              />
              <p className="u-text--muted">
                {on === "pods"
                  ? "Machine charms on LXD: Amafu runs a cluster of pod VMs and Juju puts the controller and every unit in its own container on them."
                  : readyClusters.length > 0
                    ? "Kubernetes charms: models are namespaces and units are pods on the cluster."
                    : "No Kubernetes cluster of this project is ready yet; create one on the Kubernetes page first."}
              </p>
            </>
          ),
        },
        {
          title: "Placement",
          summary: on === "pods" ? `${pods} × ${shape} pod${pods === 1 ? "" : "s"}` : `on ${hostCluster}`,
          isReady: isPlaced,
          content:
            on === "pods" ? (
              <div className="field-pair">
                <div>
                  <NumberField id="controller-pods" label="Pods" value={pods} minimum={1} maximum={MAXIMUM_PODS} onChange={setPods} help={podProblem ?? "Each pod is a VM in the LXD cluster; Juju sees each one as a zone."} />
                  <ShapeSelect id="controller-shape" label="Pod size" value={shape} onChange={setShape} />
                  <Switch label="High availability (3 controllers)" checked={isHighlyAvailable} onChange={() => setHighlyAvailable(!isHighlyAvailable)} />
                </div>
                <div>
                  <PlacementFields prefix="controller" placement={where} choices={choices} onChange={setPlacement} />
                </div>
              </div>
            ) : (
              <SearchSelect
                id="controller-cluster"
                label="Kubernetes cluster"
                value={hostCluster}
                onChange={setCluster}
                choices={readyClusters.map((value) => ({ value, label: value }))}
              />
            ),
        },
        {
          title: "Review",
          isReady: isPlaced && Boolean(firstModel),
          content: (
            <>
              <Input id="controller-model" type="text" label={on === "pods" ? "First model" : "First model (namespace)"} value={firstModel} onChange={(event) => setFirstModel(event.target.value)} />
              <p className="u-text--muted">Sign-in uses Amafu accounts; access follows each person's role in this project.</p>
              <Command
                words={[
                  "amafu", "juju", "create", name || "NAME", "--version", version, "--on", on,
                  ...(on === "pods"
                    ? [
                        pods !== 1 && "--pods", pods !== 1 && String(pods),
                        isHighlyAvailable && "--controllers", isHighlyAvailable && "3", "--shape", shape,
                        "--network", where.network || "NETWORK", "--storage-backend", where.storageBackend || "BACKEND",
                        "--external-network", where.externalNetwork || "NETWORK",
                      ]
                    : ["--cluster", hostCluster || "CLUSTER"]),
                  firstModel !== "default" && "--first-model", firstModel !== "default" && firstModel,
                ]}
              />
            </>
          ),
        },
      ]}
    />
  );
}

export function DeployPanel({ controller, onClose }: { controller: JujuController; onClose: () => void }) {
  const project = useProject();
  const models = (controller.status.models ?? []).map((model) => model.name);
  const [model, setModel] = useState(models[0] ?? "");
  const [charm, setCharm] = useState("");
  const [application, setApplication] = useState("");
  const [channel, setChannel] = useState("");
  const [units, setUnits] = useState(1);
  const [expose, setExpose] = useState(false);
  const { isBusy, problem, submit } = useSubmission();
  const name = application || charm;
  const deployment = { model, application: name, charm, units, expose, ...(channel ? { channel } : {}) };
  return (
    <Panel
      title={`Deploy a charm to ${controller.metadata.name}`}
      onClose={onClose}
      problem={problem}
      onSubmit={() => void submit(() => requestJujuChange(project, controller.metadata.name, { kind: "deploy", deployment }), onClose)}
      footer={<SubmitFooter label="Deploy" isBusy={isBusy || !charm || !model} onCancel={onClose} />}
    >
      <Select id="deploy-model" label="Model" value={model} onChange={(event) => setModel(event.target.value)} options={models.map((value) => ({ value, label: value }))} />
      <Input id="deploy-charm" type="text" label="Charm from Charmhub" required value={charm} onChange={(event) => setCharm(event.target.value)} />
      <Input id="deploy-application" type="text" label="Application name" placeholder={charm} value={application} onChange={(event) => setApplication(event.target.value)} />
      <Input id="deploy-channel" type="text" label="Channel" placeholder="latest/stable" value={channel} onChange={(event) => setChannel(event.target.value)} />
      <NumberField id="deploy-units" label="Units" value={units} minimum={1} maximum={MAXIMUM_UNITS} onChange={setUnits} help="Juju places each unit in its own container, spread over the pods." />
      <Switch label="Expose through Juju" checked={expose} onChange={() => setExpose(!expose)} />
      <Command
        words={[
          "amafu", "juju", "deploy", controller.metadata.name, "--model", model || "MODEL", "--application", name || "APPLICATION",
          "--charm", charm || "CHARM", channel && "--channel", channel, "--units", String(units), expose && "--expose",
        ]}
      />
    </Panel>
  );
}

export function ScalePodsPanel({ controller, onClose }: { controller: JujuController; onClose: () => void }) {
  const project = useProject();
  const substrate = controller.specification.substrate;
  const current = substrate.on === "pods" ? substrate.pods : 1;
  const controllers = substrate.on === "pods" ? substrate.controllers : 1;
  const [pods, setPods] = useState(current);
  const { isBusy, problem, submit } = useSubmission();
  const podProblem = !isValidPodCount(pods)
    ? `Use 1 pod, or 3 to ${MAXIMUM_PODS} so the LXD cluster keeps a quorum.`
    : pods < controllers
      ? `The controller runs ${controllers} controllers, so it needs at least ${controllers} pods.`
      : undefined;
  return (
    <Panel
      title={`Scale the pods of ${controller.metadata.name}`}
      onClose={onClose}
      problem={problem}
      onSubmit={() => void submit(() => requestJujuChange(project, controller.metadata.name, { kind: "scale-pods", pods }), onClose)}
      footer={<SubmitFooter label="Scale pods" isBusy={isBusy || pods === current || Boolean(podProblem)} onCancel={onClose} />}
    >
      <NumberField
        id="scale-pods"
        label="Pods"
        value={pods}
        minimum={1}
        maximum={MAXIMUM_PODS}
        onChange={setPods}
        help={podProblem ?? "New pods join the LXD cluster as new zones. Only pods that run no containers can be retired."}
      />
      <Command words={["amafu", "juju", "scale-pods", controller.metadata.name, "--pods", String(pods)]} />
    </Panel>
  );
}

export function AddModelPanel({ controller, onClose }: { controller: JujuController; onClose: () => void }) {
  const project = useProject();
  const [model, setModel] = useState("");
  const { isBusy, problem, submit } = useSubmission();
  const problemWithName = nameProblem(model, (controller.status.models ?? []).map((known) => known.name));
  return (
    <Panel
      title={`Add a model to ${controller.metadata.name}`}
      onClose={onClose}
      problem={problem}
      onSubmit={() => void submit(() => requestJujuChange(project, controller.metadata.name, { kind: "add-model", model }), onClose)}
      footer={<SubmitFooter label="Add model" isBusy={isBusy || !model || Boolean(problemWithName)} onCancel={onClose} />}
    >
      <Input id="model-name" type="text" label="Model name" required value={model} error={problemWithName} onChange={(event) => setModel(event.target.value)} />
      <Command words={["amafu", "juju", "add-model", controller.metadata.name, model || "MODEL"]} />
    </Panel>
  );
}

export function UpgradeControllerPanel({ controller, onClose }: { controller: JujuController; onClose: () => void }) {
  const project = useProject();
  const newer = VERSIONS.filter((candidate) => candidate.localeCompare(controller.specification.version, undefined, { numeric: true }) > 0);
  const [version, setVersion] = useState(newer[newer.length - 1] ?? "");
  const { isBusy, problem, submit } = useSubmission();
  return (
    <Panel
      title={`Upgrade ${controller.metadata.name}`}
      onClose={onClose}
      problem={problem}
      onSubmit={() => void submit(() => requestJujuChange(project, controller.metadata.name, { kind: "upgrade", version }), onClose)}
      footer={<SubmitFooter label="Upgrade" isBusy={isBusy || !version} onCancel={onClose} />}
    >
      {newer.length === 0 ? (
        <Notification severity="information">{controller.metadata.name} runs the newest version Amafu offers.</Notification>
      ) : (
        <Select id="upgrade-version" label="Upgrade to" value={version} onChange={(event) => setVersion(event.target.value)} options={newer.map((value) => ({ value, label: value }))} />
      )}
      <p className="u-text--muted">
        The controller agents upgrade first, then the agents in each model. Workloads keep running; units do not restart.
      </p>
      <Command words={["amafu", "juju", "upgrade", controller.metadata.name, "--version", version || "VERSION"]} />
    </Panel>
  );
}

export function ControllerChangeConfirmation({
  controller,
  title,
  consequence,
  label,
  request,
  onClose,
}: {
  controller: JujuController;
  title: string;
  consequence: string;
  label: string;
  request: JujuRequest;
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
      onConfirm={() => void submit(() => requestJujuChange(project, controller.metadata.name, request), onClose)}
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


