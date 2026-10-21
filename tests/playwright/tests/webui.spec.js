const { test, expect } = require("@playwright/test");
const { spawn } = require("child_process");
const http = require("http");
const fs = require("fs");
const os = require("os");
const path = require("path");

// End-to-end test of the hub's built-in web client (crates/agentlight-server/src/client.html)
// against the real server binary. Start the server, ingest events, then drive the
// browser and check rendering + the live SSE update.
const repo = path.resolve(__dirname, "..", "..", "..");
const SERVER =
  process.env.AGENTLIGHT_SERVER ||
  [
    path.join(repo, "target", "ci", "agentlight-server.exe"),
    path.join(repo, "target", "release", "agentlight-server.exe"),
    path.join(repo, "target", "debug", "agentlight-server"),
    path.join(repo, "target", "release", "agentlight-server"),
    path.join(repo, "target", "ci", "agentlight-server"),
    "C:\\SandboxOutput\\build\\agentlight-server.exe",
  ].find((candidate) => fs.existsSync(candidate));
if (!SERVER) throw new Error("agentlight-server not found; set AGENTLIGHT_SERVER");
const PORT = Number(process.env.AGENTLIGHT_TEST_PORT || 4399);
const BASE = `http://127.0.0.1:${PORT}`;

let server;

function waitForHealth(timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  return new Promise((resolve, reject) => {
    const attempt = () => {
      const req = http.get(`${BASE}/healthz`, (res) => {
        res.resume();
        if (res.statusCode === 200) return resolve();
        retry();
      });
      req.on("error", retry);
      req.setTimeout(1000, () => req.destroy());
    };
    const retry = () => {
      if (Date.now() > deadline) return reject(new Error("server did not become healthy"));
      setTimeout(attempt, 200);
    };
    attempt();
  });
}

test.beforeAll(async () => {
  const tmp = os.tmpdir();
  const pushFile = path.join(tmp, "agentlight-webui-push.json");
  const devicesFile = path.join(tmp, "agentlight-webui-devices.json");
  fs.rmSync(pushFile, { force: true });
  fs.rmSync(devicesFile, { force: true });
  server = spawn(SERVER, [], {
    env: {
      ...process.env,
      AGENTLIGHT_SOURCE: "events",
      AGENTLIGHT_BIND: `127.0.0.1:${PORT}`,
      AGENTLIGHT_TOKEN: "",
      AGENTLIGHT_STATE_FILE: "",
      AGENTLIGHT_EVENTS_FILE: pushFile,
      AGENTLIGHT_DEVICES_FILE: devicesFile,
    },
    stdio: "ignore",
  });
  await waitForHealth(15000);
});

test.afterAll(async () => {
  if (server && !server.killed) server.kill();
});

async function ingest(events) {
  const response = await fetch(`${BASE}/api/v1/ingest`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ events }),
  });
  if (!response.ok) throw new Error(`ingest failed: ${response.status}`);
}

test("healthz advertises schema 1 and ingest capabilities", async ({ request }) => {
  const res = await request.get(`${BASE}/healthz`);
  expect(res.ok()).toBeTruthy();
  const body = await res.json();
  expect(body.schema_version).toBe(1);
  expect(body.capabilities.ingest).toEqual(["upsert", "snapshot"]);
});

test("web client renders an ingested session, its Open link, and live updates", async ({ page }) => {
  await ingest([
    {
      session_id: "w1",
      status: "needs_help",
      name: "Web session",
      harness: "opencode",
      project_path: "/work/agentlight",
      url: "http://opencode-home/session/w1",
      producer: "p1",
    },
  ]);

  await page.goto(`${BASE}/`);
  // The server is open (no token, no paired devices), so the client must skip
  // the pairing form and show state directly.
  await expect(page.locator("#pair-panel")).toBeHidden();
  await expect(page.locator("#state")).toBeVisible();
  await expect(page.locator("#sessions li")).toHaveCount(1);
  await expect(page.locator("#sessions .name")).toHaveText("Web session");
  await expect(page.locator("#sessions .meta")).toHaveText("needs help");

  const open = page.locator("#sessions a.open");
  await expect(open).toBeVisible();
  await expect(open).toHaveAttribute("href", "http://opencode-home/session/w1");

  // The head light reflects the aggregate and the status line counts sessions.
  await expect(page.locator("#light")).toHaveClass(/red/);
  await expect(page.locator("#status")).toContainText("red");
  await expect(page.locator("#status")).toContainText("1 sessions");

  // Live SSE update: a new event should appear without reloading.
  await ingest([{ session_id: "w2", status: "active", name: "Second", producer: "p1" }]);
  await expect(page.locator("#sessions li")).toHaveCount(2, { timeout: 5000 });
  await expect(page.locator("#status")).toContainText("2 sessions");
});

test("web client shows no Open link for a session without a url", async ({ page }) => {
  await ingest([
    { session_id: "w3", status: "inactive", name: "No link", producer: "p1" },
  ]);
  await page.goto(`${BASE}/`);
  const row = page.locator("#sessions li", { hasText: "No link" });
  await expect(row).toHaveCount(1);
  await expect(row.locator("a.open")).toHaveCount(0);
});
