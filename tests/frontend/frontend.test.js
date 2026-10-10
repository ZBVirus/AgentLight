// Frontend behaviour tests that run anywhere Node + jsdom run (no browser), so
// they can run in the Linux container and in CI. They drive the real dist/
// modules with a mocked window.__TAURI__. Native window behaviour is not here:
// see tests/windows/ui-harness.ps1 (Win32 + UI Automation) and tests/playwright.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { JSDOM } from "jsdom";

const here = path.dirname(fileURLToPath(import.meta.url));
const dist = path.resolve(here, "..", "..", "dist");
const html = readFileSync(path.join(dist, "index.html"), "utf8");

const state = { config: null, snapshot: null, calls: [] };

function baseConfig(overrides = {}) {
  return {
    state_path: null,
    always_on_top: true,
    yellow_mode: "any_inactive",
    collapse_style: "single",
    mini_red: null,
    mini_orange: null,
    mini_green: null,
    mini_show_labels: false,
    poll_ms: 1500,
    show_done: false,
    source_kind: "hub",
    hub_url: "http://127.0.0.1:8787",
    hub_token: null,
    notifications: true,
    notification_trigger: ["needs_help", "done"],
    alarms_enabled: false,
    alarm_trigger: [],
    alarm_sound: null,
    alarm_volume: 70,
    start_at_login: false,
    server_enabled: false,
    server_bind: "127.0.0.1:8787",
    server_token_hash: null,
    session_link: "opencode_v2",
    session_link_base: null,
    ...overrides,
  };
}

function baseSnapshot(overrides = {}) {
  return {
    ok: true,
    error: null,
    state_path: "/home/me/.claude/clawlight/state.json",
    source_kind: "hub",
    source_label: "http://127.0.0.1:8787",
    exists: true,
    aggregate: "green",
    counts: { needs_help: 0, active: 1, inactive: 0, done: 0, total: 1 },
    sessions: [
      {
        session_id: "s1",
        name: "Fix auth",
        status: "active",
        status_label: "working",
        project_name: "agentlight",
        project_path: "/work/agentlight",
        harness: "opencode",
        harness_badge: "op",
        last_updated: new Date().toISOString(),
        is_done: false,
        url: "http://opencode-home/session/s1",
      },
    ],
    yellow_mode: "any_inactive",
    generated_at: new Date().toISOString(),
    ...overrides,
  };
}

// The first window exists only so ipc.js captures our dispatcher; later tests
// swap document/window, and every module reads them at call time.
const first = new JSDOM(html, { url: "http://localhost/" });
first.window.__TAURI__ = {
  core: {
    invoke: async (cmd, args) => {
      state.calls.push({ cmd, args });
      if (cmd === "get_config") return state.config;
      if (cmd === "get_snapshot") return state.snapshot;
      if (cmd === "set_config") return state.config;
      if (cmd === "get_server_status")
        return { enabled: false, url: null, admin_token_set: false, pairing_code: null, pairing_expires_at: null, devices: [] };
      if (cmd === "clear_session") return true;
      if (cmd === "clear_done") return 0;
      return null;
    },
  },
  event: { listen: async () => () => {} },
  window: { getCurrentWindow: () => ({ startDragging: async () => {} }) },
};
global.window = first.window;
global.document = first.window.document;

const store = await import(pathToFileURL(path.join(dist, "lib", "store.js")).href);
const settings = await import(pathToFileURL(path.join(dist, "views", "settings.js")).href);
const drag = await import(pathToFileURL(path.join(dist, "lib", "drag.js")).href);

async function boot({ config = baseConfig(), snapshot = baseSnapshot() } = {}) {
  const dom = new JSDOM(html, { url: "http://localhost/" });
  global.window = dom.window;
  global.document = dom.window.document;
  state.config = config;
  state.snapshot = snapshot;
  state.calls = [];
  store.setConfig(null);
  store.setSnapshot(null);
  await store.loadConfig();
  await store.refreshSnapshot();
  settings.populateSettings();
  return dom;
}

const doc = () => global.document;
const byId = (id) => global.document.getElementById(id);
const tick = () => new Promise((resolve) => setTimeout(resolve, 10));

test("collapsed view applies the selected style", async () => {
  const dom = await boot();
  assert.equal(dom.window.document.body.dataset.view, "mini");
  assert.equal(dom.window.document.body.dataset.collapse, "single");
});

test("triggers populate as a multi-select", async () => {
  await boot();
  assert.equal(byId("set-notification-needs-help").checked, true);
  assert.equal(byId("set-notification-done").checked, true);
  assert.equal(byId("set-notification-any").checked, false);
  assert.equal(byId("set-alarm-needs-help").checked, false);
  assert.equal(byId("set-alarm-done").checked, false);
  assert.equal(byId("set-alarm-any").checked, false);
});

test("saving collects every selected trigger", async () => {
  await boot({ config: baseConfig({ alarms_enabled: true }) });
  byId("set-notification-any").checked = true;
  byId("set-alarm-needs-help").checked = true;
  await settings.saveSettings();
  const saved = [...state.calls].reverse().find((c) => c.cmd === "set_config");
  assert.ok(saved, "expected a set_config call");
  assert.deepEqual(saved.args.config.notification_trigger, ["needs_help", "done", "any_status"]);
  assert.deepEqual(saved.args.config.alarm_trigger, ["needs_help"]);
});

test("notify and alarm rows hide while disabled", async () => {
  await boot({ config: baseConfig({ notifications: false, alarms_enabled: false }) });
  assert.equal(byId("set-notification-rows").classList.contains("hidden"), true);
  assert.equal(byId("set-alarm-rows").classList.contains("hidden"), true);
});

test("notify and alarm rows show while enabled", async () => {
  await boot({ config: baseConfig({ notifications: true, alarms_enabled: true }) });
  assert.equal(byId("set-notification-rows").classList.contains("hidden"), false);
  assert.equal(byId("set-alarm-rows").classList.contains("hidden"), false);
});

test("choose-state-file is hidden for a hub source and shown for a file source", async () => {
  await boot({
    snapshot: baseSnapshot({ ok: false, exists: false, source_kind: "hub", error: "Waiting for the hub" }),
  });
  assert.equal(byId("btn-pick").classList.contains("hidden"), true);

  await boot({
    config: baseConfig({ source_kind: "file" }),
    snapshot: baseSnapshot({ ok: false, exists: false, source_kind: "file", error: "Waiting for clawlight state file" }),
  });
  assert.equal(byId("btn-pick").classList.contains("hidden"), false);
});

test("detail rows render and Open invokes open_url", async () => {
  await boot();
  store.setView("detail");
  const rows = doc().querySelectorAll("#sessions li.session");
  assert.equal(rows.length, 1);
  const open = doc().querySelector("#sessions .open");
  assert.ok(open, "expected an Open button");
  assert.equal(open.title, "http://opencode-home/session/s1");
  open.dispatchEvent(new global.window.MouseEvent("click", { bubbles: true }));
  await tick();
  const call = [...state.calls].reverse().find((c) => c.cmd === "open_url");
  assert.ok(call, "expected open_url");
  assert.equal(call.args.url, "http://opencode-home/session/s1");
});

test("a session without a url has no Open button", async () => {
  const snapshot = baseSnapshot();
  snapshot.sessions[0].url = null;
  await boot({ snapshot });
  store.setView("detail");
  assert.equal(doc().querySelectorAll("#sessions .open").length, 0);
});

test("show-every-done carries an explanatory hint", () => {
  assert.match(html, /Show every done session[\s\S]{0,200}newest 5/);
});

test("any-status acts as a select-all in its trigger row", async () => {
  await boot({
    config: baseConfig({
      notifications: true,
      notification_trigger: [],
      alarms_enabled: true,
      alarm_trigger: [],
    }),
  });
  assert.equal(byId("set-notification-needs-help").checked, false);
  const any = byId("set-notification-any");
  any.checked = true;
  any.dispatchEvent(new global.window.Event("change"));
  assert.equal(byId("set-notification-needs-help").checked, true);
  assert.equal(byId("set-notification-done").checked, true);

  // Clearing the select-all clears the specific triggers too.
  any.checked = false;
  any.dispatchEvent(new global.window.Event("change"));
  assert.equal(byId("set-notification-needs-help").checked, false);
  assert.equal(byId("set-notification-done").checked, false);
});

test("loading any_status selects every trigger in the row", async () => {
  await boot({ config: baseConfig({ notification_trigger: ["any_status"] }) });
  assert.equal(byId("set-notification-needs-help").checked, true);
  assert.equal(byId("set-notification-done").checked, true);
  assert.equal(byId("set-notification-any").checked, true);
});

test("session link setting populates, hides its base for producer mode, and saves", async () => {
  await boot({
    config: baseConfig({ session_link: "opencode_v1", session_link_base: "http://localhost:4096" }),
  });
  assert.equal(byId("set-session-link").value, "opencode_v1");
  assert.equal(byId("set-session-link-base").value, "http://localhost:4096");
  assert.equal(byId("set-session-link-base-field").classList.contains("hidden"), false);

  byId("set-session-link").value = "producer";
  byId("set-session-link").dispatchEvent(new global.window.Event("change"));
  assert.equal(byId("set-session-link-base-field").classList.contains("hidden"), true);

  await settings.saveSettings();
  const saved = [...state.calls].reverse().find((c) => c.cmd === "set_config");
  assert.equal(saved.args.config.session_link, "producer");
});

test("alarm volume populates and saves", async () => {
  await boot({ config: baseConfig({ alarms_enabled: true, alarm_volume: 40 }) });
  assert.equal(byId("set-alarm-volume").value, "40");
  assert.equal(byId("set-alarm-volume-value").textContent, "40%");
  byId("set-alarm-volume").value = "85";
  await settings.saveSettings();
  const saved = [...state.calls].reverse().find((c) => c.cmd === "set_config");
  assert.equal(saved.args.config.alarm_volume, 85);
});

test("changing Always on top applies immediately", async () => {
  await boot({ config: baseConfig({ always_on_top: true }) });
  const top = byId("set-top");
  assert.equal(top.checked, true);
  top.checked = false;
  top.dispatchEvent(new global.window.Event("change"));
  await tick();
  const call = [...state.calls].reverse().find((c) => c.cmd === "set_always_on_top");
  assert.ok(call, "expected set_always_on_top");
  assert.equal(call.args.enabled, false);
});

test("collapsed right-click menu offers layouts and persists the choice", async () => {
  const dom = await boot({ config: baseConfig({ collapse_style: "single" }) });
  store.setView("mini");
  drag.wireMini();
  const mini = dom.window.document.getElementById("view-mini");
  mini.dispatchEvent(
    new dom.window.MouseEvent("contextmenu", { bubbles: true, clientX: 5, clientY: 5 }),
  );
  const items = [...dom.window.document.querySelectorAll("#mini-context-menu .context-item")];
  const labels = items.map((item) => item.textContent.replace(/[\u2713\u2003]/g, "").trim());
  assert.deepEqual(labels, ["Single light", "Horizontal lights", "Vertical lights", "Hide"]);
  assert.match(items[0].textContent, /\u2713/, "current layout is marked");

  items[1].dispatchEvent(new dom.window.MouseEvent("click", { bubbles: true }));
  await tick();
  const saved = [...state.calls].reverse().find((c) => c.cmd === "set_config");
  assert.ok(saved, "expected set_config");
  assert.equal(saved.args.config.collapse_style, "triple");
  assert.equal(saved.args.config.always_on_top, true, "other config fields are preserved");
});
