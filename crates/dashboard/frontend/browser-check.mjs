import { mkdirSync } from "node:fs";
import { join } from "node:path";

import { chromium } from "playwright";

const [, , base, screenshots] = process.argv;
const ADMIN_TOKEN = "amafu-demo-admin";
const MEMBER_TOKEN = "amafu-demo-member";
const VIEWER_TOKEN = "amafu-demo-viewer";
const PROJECT = "default";
const SETTLE_MILLISECONDS = 20_000;

const PAGES = [
  ["overview", "/", "Cluster"],
  ["virtual-machines", "/virtual-machines", "web-1"],
  ["containers", "/containers", "nginx-edge"],
  ["images", "/images", "acme-api"],
  ["storage", "/storage", "ceph-prod"],
  ["volumes", "/volumes", "db-primary-data"],
  ["networks", "/networks", "10.20.0.0/24"],
  ["floating-ips", "/floating-ips", "203.0.113.10"],
  ["security-groups", "/security-groups", "monitoring"],
  ["nodes", "/nodes", "node-9"],
  ["operations", "/operations", "migrate-db-replica-77c2"],
  ["settings", "/settings", "26.04.1"],
];

const failures = [];

function check(condition, description) {
  if (!condition) {
    failures.push(description);
    console.error(`FAILED: ${description}`);
  } else {
    console.log(`ok: ${description}`);
  }
}

async function attempt(description, action) {
  try {
    await action();
    check(true, description);
  } catch (error) {
    check(false, `${description}: ${error.message.split("\n")[0]}`);
  }
}

async function session(browser, token, colorScheme = "light") {
  const context = await browser.newContext({ viewport: { width: 1440, height: 960 }, colorScheme });
  const page = await context.newPage();
  const problems = [];
  page.on("pageerror", (error) => problems.push(error.message));
  page.on("console", (message) => {
    if (message.type() === "error" && !message.text().includes("Failed to load resource")) {
      problems.push(message.text());
    }
  });
  await page.goto(base);
  await page.getByLabel("Token").fill(token);
  await page.getByLabel("Project").fill(PROJECT);
  await page.getByRole("button", { name: "Sign in" }).click();
  await page.getByRole("link", { name: "Virtual machines" }).first().waitFor();
  return { context, page, problems };
}

function row(page, name) {
  return page.locator("tr", { has: page.getByText(name, { exact: true }) }).first();
}

async function shoot(page, name) {
  await page.waitForTimeout(300);
  await page.screenshot({ path: join(screenshots, `${name}.png`) });
}

async function visitEveryPage(browser) {
  const { context, page, problems } = await session(browser, ADMIN_TOKEN);
  for (const [name, path, expected] of PAGES) {
    await attempt(`the ${name} page shows ${expected}`, async () => {
      await page.goto(`${base}${path}`);
      await page.getByText(expected, { exact: false }).first().waitFor({ timeout: 10_000 });
      await shoot(page, name);
    });
  }
  check(problems.length === 0, `no script errors while visiting pages ${problems.join(" | ")}`);
  await context.close();
}

async function darkTheme(browser) {
  const { context, page } = await session(browser, ADMIN_TOKEN, "dark");
  await attempt("the overview follows a dark system theme", async () => {
    await page.getByText("Capacity by zone").waitFor();
    check(await page.evaluate(() => document.body.classList.contains("is-dark")), "the body carries the dark theme");
    await shoot(page, "overview-dark");
  });
  await context.close();
}

async function workloads(browser) {
  const { context, page, problems } = await session(browser, ADMIN_TOKEN);

  await attempt("a container created in the panel starts running", async () => {
    await page.goto(`${base}/containers`);
    await page.getByRole("button", { name: "Create container" }).click();
    await page.getByLabel("Name").fill("api-3");
    await page.getByLabel("Image").selectOption("acme-api");
    await page.getByText(/^app ·/).click();
    await page.getByLabel("Environment").fill("PORT=8080");
    await page.getByText("--env PORT=8080").waitFor();
    await shoot(page, "containers-create");
    await page.getByRole("button", { name: "Create", exact: true }).click();
    await row(page, "api-3").getByText("Running").waitFor({ timeout: SETTLE_MILLISECONDS });
  });

  await attempt("stopping a VM from its row stops it", async () => {
    await page.goto(`${base}/virtual-machines`);
    await page.getByRole("button", { name: "Stop web-1" }).click();
    await row(page, "web-1").getByText("Stopped").waitFor({ timeout: SETTLE_MILLISECONDS });
    await page.getByRole("button", { name: "Start web-1" }).click();
    await row(page, "web-1").getByText("Running").waitFor({ timeout: SETTLE_MILLISECONDS });
  });

  await attempt("a migration started from the panel finishes", async () => {
    await page.goto(`${base}/virtual-machines`);
    await page.getByRole("button", { name: "Migrate web-2" }).click();
    await page.getByLabel("Destination").selectOption("node-5");
    await page.getByText("--to node-5").waitFor();
    await shoot(page, "virtual-machines-migrate");
    await page.getByRole("button", { name: "Migrate", exact: true }).click();
    await row(page, "web-2").getByText("Migrating").waitFor({ timeout: SETTLE_MILLISECONDS });
    await shoot(page, "virtual-machines-migrating");
    await row(page, "web-2").getByText("node-5").waitFor({ timeout: SETTLE_MILLISECONDS });
    await page.goto(`${base}/operations`);
    await page.locator("tr", { hasText: "web-2" }).first().getByText("Succeeded").waitFor({ timeout: SETTLE_MILLISECONDS });
  });

  await attempt("a VM detail panel shows its interfaces", async () => {
    await page.goto(`${base}/virtual-machines?panel=detail&subject=web-1`);
    await page.getByText("amafu vm show web-1").waitFor();
    await shoot(page, "virtual-machines-detail");
  });

  await attempt("a rule added in the security group panel is kept", async () => {
    await page.goto(`${base}/security-groups?panel=detail&subject=web`);
    await page.getByLabel("Ports").fill("8443");
    await page.getByRole("button", { name: "Add rule" }).click();
    await page.locator(".amafu-panel").getByText("TCP 8443", { exact: true }).waitFor({ timeout: SETTLE_MILLISECONDS });
    await shoot(page, "security-groups-detail");
  });

  await attempt("a deleted volume disappears", async () => {
    await page.goto(`${base}/volumes`);
    await page.getByRole("button", { name: "Delete scratch" }).click();
    await page.getByRole("button", { name: "Delete", exact: true }).click();
    await row(page, "scratch").waitFor({ state: "detached", timeout: SETTLE_MILLISECONDS });
  });

  await attempt("a paused operation resumes and finishes", async () => {
    await page.goto(`${base}/operations`);
    await page.getByRole("button", { name: "Resume migrate-db-replica-77c2" }).click();
    await row(page, "migrate-db-replica-77c2").getByText("Succeeded").waitFor({ timeout: SETTLE_MILLISECONDS });
  });

  await attempt("a floating IP moves to another VM", async () => {
    await page.goto(`${base}/floating-ips`);
    await page.getByRole("button", { name: "Move spare" }).click();
    await page.getByLabel("Attach to").selectOption("web-3");
    await page.getByRole("button", { name: "Move", exact: true }).click();
    await row(page, "spare").getByText("Bound").waitFor({ timeout: SETTLE_MILLISECONDS });
  });

  await attempt("an invalid network is refused with the cluster's reason", async () => {
    await page.goto(`${base}/networks`);
    await page.getByRole("button", { name: "Create network" }).click();
    await page.getByLabel("Name").fill("broken");
    await page.getByLabel("Subnet").fill("10.0.0.0/33");
    await page.getByRole("button", { name: "Create", exact: true }).click();
    await page.getByText("The cluster refused").waitFor();
    await shoot(page, "networks-refused");
  });

  check(problems.length === 0, `no script errors during the workload flows ${problems.join(" | ")}`);
  await context.close();
}

async function restrictedRoles(browser) {
  const member = await session(browser, MEMBER_TOKEN);
  await attempt("a member is told nodes are not visible", async () => {
    await member.page.goto(`${base}/nodes`);
    await member.page.getByText("Not visible to you").waitFor({ timeout: 10_000 });
  });
  await member.context.close();

  const viewer = await session(browser, VIEWER_TOKEN);
  await attempt("a viewer's change is refused", async () => {
    await viewer.page.goto(`${base}/virtual-machines`);
    await viewer.page.getByRole("button", { name: "Stop web-3" }).click();
    await viewer.page.getByText("The cluster refused").waitFor({ timeout: 10_000 });
  });
  await attempt("signing out returns to the sign-in page", async () => {
    await viewer.page.getByRole("button", { name: "Sign out" }).click();
    await viewer.page.getByRole("button", { name: "Sign in" }).waitFor();
  });
  await viewer.context.close();
}

mkdirSync(screenshots, { recursive: true });
const browser = await chromium.launch();
try {
  await visitEveryPage(browser);
  await darkTheme(browser);
  await workloads(browser);
  await restrictedRoles(browser);
} finally {
  await browser.close();
}
if (failures.length > 0) {
  console.error(`${failures.length} checks failed`);
  process.exit(1);
}
console.log("every check passed");
