"use strict";

const views = {
  nodes: {
    title: "Nodes",
    path: () => "/api/v1/nodes",
    columns: ["Name", "Phase", "Roles", "Image", "Cordoned", ""],
    row: (node) => [
      node.metadata.name,
      node.status.phase,
      (node.specification.roles || []).join(", "),
      node.status.boot && node.status.boot.booted_image ? node.status.boot.booted_image.version : "-",
      node.specification.is_cordoned ? "yes" : "no",
      action(node.specification.is_cordoned ? "Uncordon" : "Cordon", () => setCordoned(node, !node.specification.is_cordoned)),
    ],
  },
  vms: {
    title: "VMs",
    path: () => `/api/v1/vms${projectQuery()}`,
    columns: ["Name", "Phase", "Node", "CPUs", "Memory", ""],
    row: (vm) => [
      vm.metadata.name,
      vm.status.phase,
      vm.status.node || "-",
      vm.specification.cpu_count,
      `${vm.specification.memory_mebibytes} MiB`,
      action("Delete", () => removeVm(vm)),
    ],
  },
  operations: {
    title: "Operations",
    path: () => "/api/v1/operations",
    columns: ["Name", "Kind", "Progress", "Outcome"],
    row: (operation) => [
      operation.metadata.name,
      operation.specification.kind,
      `${operation.status.completed_steps || 0}/${operation.specification.steps.length}`,
      operation.status.outcome ? operation.status.outcome.outcome : "running",
    ],
  },
  upgrade: {
    title: "Upgrade",
    path: () => "/api/v1/nodes",
    columns: ["Node", "Runs", "Slot", "Confirmed", "Inactive slot"],
    row: (node) => {
      const boot = node.status.boot || {};
      return [
        node.metadata.name,
        boot.booted_image ? boot.booted_image.version : "-",
        boot.booted_slot || "-",
        boot.booted_slot && boot.booted_slot === boot.default_slot ? "yes" : "no",
        boot.inactive_image ? boot.inactive_image.version : "-",
      ];
    },
  },
};

let project = null;
let current = "nodes";

function element(name) {
  return document.getElementById(name);
}

function projectQuery() {
  return project ? `?project=${encodeURIComponent(project)}` : "";
}

function action(label, perform) {
  const button = document.createElement("button");
  button.textContent = label;
  button.addEventListener("click", perform);
  return button;
}

async function call(method, path, body) {
  const response = await fetch(path, {
    method,
    credentials: "same-origin",
    headers: { "content-type": "application/json", "x-amafu-dashboard": "1" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (response.status === 401) {
    showSignIn();
    throw new Error("Your session has ended; sign in again.");
  }
  const text = await response.text();
  const answer = text ? JSON.parse(text) : null;
  if (!response.ok) {
    throw new Error(answer && answer.message ? answer.message : `The cluster answered ${response.status}`);
  }
  return answer;
}

async function setCordoned(node, isCordoned) {
  const specification = { ...node.specification, is_cordoned: isCordoned };
  const change = { specification, expected_generation: node.metadata.generation };
  await guarded(() => call("PUT", `/api/v1/nodes/${node.metadata.name}`, change));
}

async function removeVm(vm) {
  if (confirm(`Delete ${vm.metadata.name}?`)) {
    await guarded(() => call("DELETE", `/api/v1/vms/${vm.metadata.name}${projectQuery()}`));
  }
}

async function guarded(perform) {
  try {
    await perform();
    await render();
  } catch (error) {
    element("view-error").textContent = error.message;
  }
}

function cell(value) {
  const cell = document.createElement("td");
  if (value instanceof Node) {
    cell.appendChild(value);
  } else {
    cell.textContent = value === undefined || value === null ? "-" : String(value);
  }
  return cell;
}

async function render() {
  const view = views[current];
  element("view-title").textContent = view.title;
  element("view-error").textContent = "";
  for (const button of document.querySelectorAll("nav button[data-view]")) {
    if (button.dataset.view === current) {
      button.setAttribute("aria-current", "page");
    } else {
      button.removeAttribute("aria-current");
    }
  }
  const head = element("view-table").tHead;
  const body = element("view-table").tBodies[0];
  head.replaceChildren(rowOf(view.columns.map((column) => {
    const heading = document.createElement("th");
    heading.textContent = column;
    return heading;
  })));
  try {
    const answer = await call("GET", view.path());
    body.replaceChildren(...answer.items.map((item) => rowOf(view.row(item).map(cell))));
  } catch (error) {
    body.replaceChildren();
    element("view-error").textContent = error.message;
  }
}

function rowOf(cells) {
  const row = document.createElement("tr");
  row.replaceChildren(...cells);
  return row;
}

function showSignIn() {
  element("sign-in").hidden = false;
  element("view").hidden = true;
  element("navigation").hidden = true;
}

function showDashboard() {
  element("sign-in").hidden = true;
  element("view").hidden = false;
  element("navigation").hidden = false;
  render();
}

element("sign-in").addEventListener("submit", async (event) => {
  event.preventDefault();
  const form = new FormData(event.target);
  const chosen = form.get("project").trim();
  try {
    await call("POST", "/session", { token: form.get("token"), project: chosen || null });
    project = chosen || null;
    event.target.reset();
    showDashboard();
  } catch (error) {
    element("sign-in-error").textContent = error.message;
  }
});

element("sign-out").addEventListener("click", async () => {
  await fetch("/session", { method: "DELETE", credentials: "same-origin" });
  showSignIn();
});

for (const button of document.querySelectorAll("nav button[data-view]")) {
  button.addEventListener("click", () => {
    current = button.dataset.view;
    render();
  });
}

fetch("/session", { credentials: "same-origin" }).then(async (response) => {
  if (response.ok) {
    project = (await response.json()).project;
    showDashboard();
  } else {
    showSignIn();
  }
});

setInterval(() => {
  if (!element("view").hidden) {
    render();
  }
}, 5000);
