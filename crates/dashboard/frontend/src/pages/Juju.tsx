import { Button, MainTable } from "@canonical/react-components";

import type { JujuController, JujuControllerChange, KubernetesCluster, PlanStep } from "../cluster/environments";
import { JUJU_CHANGES, JUJU_CONTROLLERS, JUJU_LABEL, KUBERNETES_CLUSTERS, describePlanStep, isRunning } from "../cluster/environments";
import type { GuestInventory, Vm } from "../cluster/kinds";
import { useCollection } from "../cluster/live";
import { projectScope } from "../cluster/scopes";
import CollectionNotice from "../components/CollectionNotice";
import Command from "../components/Command";
import GuestPills from "../components/GuestPills";
import IconAction from "../components/IconAction";
import PageHeader from "../components/PageHeader";
import Panel from "../components/Panel";
import PanelTabs from "../components/PanelTabs";
import Status from "../components/Status";
import StepPlan, { PlanProgress, StartedAt, planPhase, sinceCreation } from "../components/StepPlan";
import SubjectLink from "../components/SubjectLink";
import { usePanel } from "../components/panels";
import type { PanelState } from "../components/panels";
import { countOf } from "../format";
import { useProject } from "../session";
import {
  AddModelPanel,
  ControllerChangeConfirmation,
  CreateControllerPanel,
  DeployPanel,
  ScalePodsPanel,
  UpgradeControllerPanel,
} from "./jujuPanels";

const describe = (step: PlanStep) => describePlanStep(step);

function substrateOf(controller: JujuController): string {
  const substrate = controller.specification.substrate;
  return substrate.on === "pods" ? `${countOf(substrate.pods, "pod")} · ${substrate.network}` : `cluster ${substrate.cluster}`;
}

function changesOf(changes: JujuControllerChange[], controller: string): JujuControllerChange[] {
  return sinceCreation(changes.filter((change) => change.specification.controller === controller));
}

function Pods({ pods, inventories }: { pods: Vm[]; inventories: GuestInventory[] }) {
  if (pods.length === 0) {
    return <p className="u-text--muted">No pods yet.</p>;
  }
  return (
    <ul className="p-list--divided">
      {pods.map((pod) => {
        const inventory = inventories.find((candidate) => candidate.metadata.name === pod.metadata.name);
        return (
          <li key={pod.metadata.name} className="p-list__item">
            <SubjectLink name={pod.metadata.name} page="/virtual-machines" /> <Status phase={pod.status.phase} />
            {inventory ? <GuestPills inventory={inventory} /> : <p className="guest-pills u-text--muted">Amafu has not reached LXD on this pod yet.</p>}
          </li>
        );
      })}
    </ul>
  );
}

function ControllerDetail({
  controller,
  changes,
  pods,
  inventories,
  panel,
}: {
  controller: JujuController;
  changes: JujuControllerChange[];
  pods: Vm[];
  inventories: GuestInventory[];
  panel: PanelState;
}) {
  const name = controller.metadata.name;
  const running = changes.find(isRunning);
  const isBusy = Boolean(running) || controller.status.phase !== "ready";
  const substrate = controller.specification.substrate;
  const isOnPods = substrate.on === "pods";
  const canEnableHa = substrate.on === "pods" && substrate.controllers === 1;
  const changeTable = (
    <MainTable
      emptyStateMsg="No changes yet."
      headers={[{ content: "Change" }, { content: "Started" }, { content: "Progress" }, { content: "Status" }]}
      rows={changes.map((change) => ({
        key: change.metadata.name,
        columns: [
          { content: change.specification.request.kind.replaceAll("-", " ") },
          { content: <StartedAt plan={change} /> },
          { content: <PlanProgress plan={change} describe={describe} /> },
          { content: <Status {...planPhase(change)} /> },
        ],
      }))}
    />
  );
  const modelTable = (
    <MainTable
      emptyStateMsg="Models appear once the controller is ready."
      headers={[{ content: "Model" }, { content: "UUID" }, { content: "Actions", className: "u-align--right" }]}
      rows={(controller.status.models ?? []).map((model) => ({
        key: model.name,
        columns: [
          { content: model.name },
          { content: <code>{model.uuid}</code> },
          {
            className: "u-align--right",
            content: <IconAction icon="delete" label={`Destroy model ${model.name}`} isDisabled={isBusy} onClick={() => panel.open("destroy-model", `${name}/${model.name}`)} />,
          },
        ],
      }))}
    />
  );
  return (
    <Panel title={name} isWide onClose={panel.close}>
      <p>
        Juju {controller.status.version ?? controller.specification.version} · {isOnPods ? "LXD cloud on Amafu pods" : "Kubernetes cloud"} · API{" "}
        {controller.status.addresses?.[0] ?? "not reachable yet"}
      </p>
      <Status phase={controller.status.phase} detail={controller.status.message} />
      <div className="row-actions panel-actions">
        <Button type="button" disabled={isBusy} onClick={() => panel.open("add-model", name)}>
          Add model
        </Button>
        <Button type="button" disabled={isBusy} onClick={() => panel.open("deploy", name)}>
          Deploy charm
        </Button>
        {isOnPods && (
          <Button type="button" disabled={isBusy} onClick={() => panel.open("scale-pods", name)}>
            Scale pods
          </Button>
        )}
        {canEnableHa && (
          <Button type="button" disabled={isBusy} onClick={() => panel.open("enable-ha", name)}>
            Enable HA
          </Button>
        )}
        {!isOnPods && (
          <Button type="button" disabled={isBusy} onClick={() => panel.open("rotate-credential", name)}>
            Rotate cluster credential
          </Button>
        )}
      </div>
      <PanelTabs
        tabs={[
          {
            label: "Overview",
            content: (
              <>
                {running ? (
                  <>
                    <h3 className="p-heading--5">Running: {running.specification.request.kind.replaceAll("-", " ")}</h3>
                    <StepPlan plan={running} describe={describe} />
                  </>
                ) : (
                  <p className="u-text--muted">No change is running.</p>
                )}
                <Command title={`Sign in to ${name} as yourself`} words={["amafu", "juju", "token", name]} />
                <p className="u-text--muted">
                  The controller trusts five-minute tokens that Amafu signs; your role in this project decides your access to its models.
                </p>
              </>
            ),
          },
          { label: `Models (${controller.status.models?.length ?? 0})`, content: modelTable },
          ...(isOnPods ? [{ label: `Pods (${pods.length})`, content: <Pods pods={pods} inventories={inventories} /> }] : []),
          { label: `Changes (${changes.length})`, content: changeTable },
        ]}
      />
    </Panel>
  );
}

function ChangePanels({ controllers, clusters, panel }: { controllers: JujuController[]; clusters: KubernetesCluster[]; panel: PanelState }) {
  const [controllerName, part] = (panel.subject ?? "").split("/");
  if (panel.panel === "create") {
    return <CreateControllerPanel taken={controllers.map((controller) => controller.metadata.name)} clusters={clusters} onClose={panel.close} />;
  }
  const controller = controllers.find((candidate) => candidate.metadata.name === controllerName);
  if (!controller) {
    return null;
  }
  const confirm = (title: string, label: string, consequence: string, request: Parameters<typeof ControllerChangeConfirmation>[0]["request"]) => (
    <ControllerChangeConfirmation controller={controller} title={title} label={label} consequence={consequence} request={request} onClose={panel.close} />
  );
  switch (panel.panel) {
    case "deploy":
      return <DeployPanel controller={controller} onClose={panel.close} />;
    case "add-model":
      return <AddModelPanel controller={controller} onClose={panel.close} />;
    case "scale-pods":
      return <ScalePodsPanel controller={controller} onClose={panel.close} />;
    case "upgrade":
      return <UpgradeControllerPanel controller={controller} onClose={panel.close} />;
    case "destroy-model":
      return confirm(`Destroy model ${part}?`, "Destroy model", "Juju removes every application in it, along with the containers its units ran in.", { kind: "destroy-model", model: part ?? "" });
    case "enable-ha":
      return confirm(`Enable high availability for ${controller.metadata.name}?`, "Enable HA", "Juju starts two more controllers in containers on other pods; it needs at least 3 pods.", { kind: "enable-ha" });
    case "rotate-credential":
      return confirm(`Rotate the cluster credential of ${controller.metadata.name}?`, "Rotate", "A new service account token replaces the one Juju uses on its cluster; the old token is deleted.", { kind: "rotate-credential" });
    case "delete":
      return confirm(`Delete ${controller.metadata.name}?`, "Delete controller", "This destroys every model, then deletes the controller and every pod, volume and address it owns.", { kind: "delete" });
    default:
      return null;
  }
}

export default function Juju() {
  const project = useProject();
  const controllers = useCollection<JujuController>(projectScope(JUJU_CONTROLLERS, project));
  const changes = useCollection<JujuControllerChange>(projectScope(JUJU_CHANGES, project));
  const clusters = useCollection<KubernetesCluster>(projectScope(KUBERNETES_CLUSTERS, project));
  const vms = useCollection<Vm>(projectScope("vms", project));
  const inventories = useCollection<GuestInventory>(projectScope("guest-inventories", project));
  const panel = usePanel();
  const subject = controllers.items.find((controller) => controller.metadata.name === panel.subject);
  const creating = changes.items.filter(
    (change) => change.specification.request.kind === "create" && isRunning(change) && !controllers.items.some((controller) => controller.metadata.name === change.specification.controller),
  );
  return (
    <div className="page">
      <PageHeader title="Juju">
        <Button appearance="positive" onClick={() => panel.open("create")}>
          Create controller
        </Button>
      </PageHeader>
      <p className="u-text--muted">
        Juju controllers on LXD pods or on a Kubernetes cluster from the Kubernetes page. On pods, Juju runs the controller and
        every unit in its own container; Amafu signs people in with their Amafu accounts.
      </p>
      <CollectionNotice state={controllers.state} noun="Juju controllers" />
      <MainTable
        responsive
        emptyStateMsg="No Juju controllers. Create one on LXD pods or on a Kubernetes cluster."
        headers={[
          { content: "Name" },
          { content: "Version" },
          { content: "Runs on" },
          { content: "Status" },
          { content: "Models" },
          { content: "API" },
          { content: "Actions", className: "u-align--right" },
        ]}
        rows={[
          ...creating.map((change) => ({
            key: `creating-${change.metadata.name}`,
            columns: [
              { content: change.specification.controller },
              { content: change.specification.request.kind === "create" ? change.specification.request.controller.version : "" },
              { content: "—" },
              { content: <Status phase="creating" detail={planPhase(change).detail} /> },
              { content: <PlanProgress plan={change} describe={describe} /> },
              { content: "—" },
              { content: "" },
            ],
          })),
          ...controllers.items.map((controller) => {
            const isChanging = changesOf(changes.items, controller.metadata.name).some(isRunning);
            return {
              key: controller.metadata.name,
              columns: [
                { content: <SubjectLink name={controller.metadata.name} /> },
                { content: controller.status.version ?? controller.specification.version },
                { content: substrateOf(controller) },
                { content: <Status phase={controller.status.phase} detail={controller.status.message} /> },
                { content: String(controller.status.models?.length ?? 0) },
                { content: controller.status.addresses?.[0] ?? "—" },
                {
                  className: "u-align--right",
                  content: (
                    <div className="row-actions">
                      <IconAction icon="change-version" label={`Upgrade ${controller.metadata.name}`} isDisabled={isChanging} onClick={() => panel.open("upgrade", controller.metadata.name)} />
                      <IconAction icon="delete" label={`Delete ${controller.metadata.name}`} isDisabled={isChanging} onClick={() => panel.open("delete", controller.metadata.name)} />
                    </div>
                  ),
                },
              ],
            };
          }),
        ]}
      />
      {panel.panel === "detail" && subject && (
        <ControllerDetail
          controller={subject}
          changes={changesOf(changes.items, subject.metadata.name)}
          pods={vms.items.filter((vm) => vm.metadata.labels?.[JUJU_LABEL] === subject.metadata.name)}
          inventories={inventories.items}
          panel={panel}
        />
      )}
      <ChangePanels controllers={controllers.items} clusters={clusters.items} panel={panel} />
    </div>
  );
}
