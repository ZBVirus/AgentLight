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

const state = { config: null, snapshot: null, calls: [], hubStatus: null };

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
    show_subagents: false,
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
    session_link_bases: {},
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
      if (cmd === "app_version") return "0.5.0 · test";
      if (cmd === "set_config") return state.config;
      if (cmd === "get_server_status")
        return { enabled: false, url: null, admin_token_set: false, pairing_code: null, pairing_expires_at: null, devices: [] };
      if (cmd === "get_hub_status")
        return { source: "hub", url: "http://127.0.0.1:8787", error: null, ...state.hubStatus };
      if (cmd === "get_hub_status_cached") return state.hubStatus;
      if (cmd === "copy_diagnostics") return null;
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

async function boot({ config = baseConfig(), snapshot = baseSnapshot(), hubStatus = { url: "http://127.0.0.1:8787", reachable: true, version: "0.4.0", schema_version: 1, error: null, checked_at: "2026-10-15 12:00:00" } } = {}) {
  const dom = new JSDOM(html, { url: "http://localhost/" });
  global.window = dom.window;
  global.document = dom.window.document;
  state.config = config;
  state.snapshot = snapshot;
  state.hubStatus = hubStatus;
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

test("settings shows the app version and build id", async () => {
  await boot();
  await tick();
  assert.match(byId("app-version").value, /^0\.5\.0/);
  assert.match(byId("app-version").value, /test/);
});

test("subagent visibility and the subagent-finished trigger save", async () => {
  await boot({
    config: baseConfig({ show_subagents: false, notifications: true, notification_trigger: [] }),
  });
  assert.equal(byId("set-show-subagents").checked, false);
  byId("set-show-subagents").checked = true;
  byId("set-notification-subagent").checked = true;
  await settings.saveSettings();
  const saved = [...state.calls].reverse().find((c) => c.cmd === "set_config");
  assert.equal(saved.args.config.show_subagents, true);
  assert.deepEqual(saved.args.config.notification_trigger, ["subagent_done"]);
});

test("session link setting populates, hides its base for producer mode, and saves", async () => {
  await boot({
    config: baseConfig({
      session_link: "opencode_v1",
      session_link_bases: { opencode_v1: "http://localhost:4096" },
    }),
  });
  assert.equal(byId("set-session-link").value, "opencode_v1");
  assert.equal(byId("set-session-link-base").value, "http://localhost:4096");
  assert.equal(byId("set-session-link-base-field").classList.contains("hidden"), false);
  assert.equal(byId("set-session-link-producer-hint").classList.contains("hidden"), true);

  byId("set-session-link").value = "producer";
  byId("set-session-link").dispatchEvent(new global.window.Event("change"));
  assert.equal(byId("set-session-link-base-field").classList.contains("hidden"), true);
  assert.equal(byId("set-session-link-producer-hint").classList.contains("hidden"), false);

  await settings.saveSettings();
  const saved = [...state.calls].reverse().find((c) => c.cmd === "set_config");
  assert.equal(saved.args.config.session_link, "producer");
  assert.deepEqual(saved.args.config.session_link_bases, {
    opencode_v1: "http://localhost:4096",
  });
});

test("each session link mode remembers its own server address", async () => {
  await boot({
    config: baseConfig({
      session_link: "opencode_v2",
      session_link_bases: { opencode_v2: "http://v2.example:4096" },
    }),
  });
  assert.equal(byId("set-session-link-base").value, "http://v2.example:4096");

  // v1 has no saved address: the input starts empty, not carrying v2's over.
  byId("set-session-link").value = "opencode_v1";
  byId("set-session-link").dispatchEvent(new global.window.Event("change"));
  assert.equal(byId("set-session-link-base").value, "");
  byId("set-session-link-base").value = "http://v1.example:4096";

  // Switching back restores v2's address; switching away again restores v1's.
  byId("set-session-link").value = "opencode_v2";
  byId("set-session-link").dispatchEvent(new global.window.Event("change"));
  assert.equal(byId("set-session-link-base").value, "http://v2.example:4096");
  byId("set-session-link").value = "opencode_v1";
  byId("set-session-link").dispatchEvent(new global.window.Event("change"));
  assert.equal(byId("set-session-link-base").value, "http://v1.example:4096");

  // Clearing an address forgets it for that mode only.
  byId("set-session-link-base").value = "";
  byId("set-session-link").value = "opencode_v2";
  byId("set-session-link").dispatchEvent(new global.window.Event("change"));
  assert.equal(byId("set-session-link-base").value, "http://v2.example:4096");
  byId("set-session-link").value = "opencode_v1";
  byId("set-session-link").dispatchEvent(new global.window.Event("change"));
  assert.equal(byId("set-session-link-base").value, "");

  // A fresh v1 address saves only what is set.
  byId("set-session-link-base").value = "http://v1b.example:4096";
  await settings.saveSettings();
  const saved = [...state.calls].reverse().find((c) => c.cmd === "set_config");
  assert.deepEqual(saved.args.config.session_link_bases, {
    opencode_v2: "http://v2.example:4096",
    opencode_v1: "http://v1b.example:4096",
  });
});

test("openchamber link mode keeps the server field and its default", async () => {
  await boot({ config: baseConfig({ session_link: "openchamber" }) });
  assert.equal(byId("set-session-link-base-field").classList.contains("hidden"), false);
  assert.equal(byId("set-session-link-base").placeholder, "http://localhost:3000");
});

test("unchecking a specific trigger clears any-status", async () => {
  await boot({
    config: baseConfig({ notifications: true, notification_trigger: ["any_status"] }),
  });
  assert.equal(byId("set-notification-any").checked, true);
  const done = byId("set-notification-done");
  done.checked = false;
  done.dispatchEvent(new global.window.Event("change"));
  assert.equal(byId("set-notification-any").checked, false);
  assert.equal(byId("set-notification-needs-help").checked, true, "the others stay");
});

test("the alarm Test button plays the current sound and volume", async () => {
  await boot({ config: baseConfig({ alarms_enabled: true, alarm_volume: 55 }) });
  byId("set-alarm-volume").value = "35";
  byId("btn-alarm-test").dispatchEvent(
    new global.window.MouseEvent("click", { bubbles: true }),
  );
  await tick();
  const call = [...state.calls].reverse().find((c) => c.cmd === "test_alarm");
  assert.ok(call, "expected test_alarm");
  assert.equal(call.args.volume, 35);
  assert.equal(call.args.sound, null);
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

test("collapsed right-click opens the native layout menu", async () => {
  const dom = await boot({ config: baseConfig({ collapse_style: "single" }) });
  store.setView("mini");
  drag.wireMini();
  const mini = dom.window.document.getElementById("view-mini");
  mini.dispatchEvent(
    new dom.window.MouseEvent("contextmenu", { bubbles: true, clientX: 5, clientY: 5 }),
  );
  await tick();
  assert.ok(
    state.calls.some((call) => call.cmd === "show_mini_menu"),
    "expected show_mini_menu to be invoked",
  );
});

test("drag regions report drag start and end to the shell", async () => {
  const dom = await boot();
  store.setView("detail");
  drag.wireMini();
  const bar = dom.window.document.querySelector("header.bar");
  bar.dispatchEvent(
    new dom.window.MouseEvent("mousedown", { bubbles: true, button: 0 }),
  );
  await tick();
  assert.ok(
    state.calls.some((call) => call.cmd === "set_dragging" && call.args.dragging === true),
    "expected set_dragging(true) on a title-bar mousedown",
  );
  dom.window.dispatchEvent(
    new dom.window.MouseEvent("mouseup", { bubbles: true, button: 0 }),
  );
  await tick();
  assert.ok(
    state.calls.some((call) => call.cmd === "set_dragging" && call.args.dragging === false),
    "expected set_dragging(false) on mouseup",
  );
});

test("Ctrl+S saves and Esc discards while Settings is open", async () => {
  await boot();
  store.setView("settings");
  const save = new global.window.KeyboardEvent("keydown", {
    key: "s",
    ctrlKey: true,
    bubbles: true,
    cancelable: true,
  });
  settings.handleSettingsKey(save);
  await tick();
  assert.ok(
    [...state.calls].some((call) => call.cmd === "set_config"),
    "Ctrl+S saves the form",
  );
  assert.equal(save.defaultPrevented, true);

  // Cmd+S (macOS) is the same shortcut.
  store.setView("settings");
  const meta = new global.window.KeyboardEvent("keydown", {
    key: "s",
    metaKey: true,
    bubbles: true,
    cancelable: true,
  });
  settings.handleSettingsKey(meta);
  await tick();
  assert.equal(meta.defaultPrevented, true, "Cmd+S saves too");

  store.setView("settings");
  const esc = new global.window.KeyboardEvent("keydown", {
    key: "Escape",
    bubbles: true,
    cancelable: true,
  });
  settings.handleSettingsKey(esc);
  assert.equal(store.view, "detail", "Esc leaves without saving");
  assert.equal(esc.defaultPrevented, true);

  store.setView("mini");
  const ignored = new global.window.KeyboardEvent("keydown", {
    key: "s",
    ctrlKey: true,
    bubbles: true,
    cancelable: true,
  });
  settings.handleSettingsKey(ignored);
  assert.equal(ignored.defaultPrevented, false, "outside Settings the shortcut is inert");
});

test("subagent rows carry a marker badge", async () => {
  const snapshot = baseSnapshot();
  snapshot.sessions.push({
    ...snapshot.sessions[0],
    session_id: "s2",
    name: "Child session",
    is_subagent: true,
  });
  await boot({ config: baseConfig({ show_subagents: true }), snapshot });
  store.setView("detail");
  assert.equal(doc().querySelectorAll("#sessions li.session").length, 2);
  const badges = doc().querySelectorAll("#sessions .badge.sub");
  assert.equal(badges.length, 1);
  assert.equal(badges[0].textContent, "sub");
});

test("hub status renders the cached result for a hub source", async () => {
  await boot({ config: baseConfig({ source_kind: "hub" }) });
  await tick();
  assert.equal(
    state.calls.some((call) => call.cmd === "get_hub_status_cached"),
    true,
    "opening Settings reads the cached result",
  );
  assert.equal(
    state.calls.some((call) => call.cmd === "get_hub_status"),
    false,
    "opening Settings must not probe the network",
  );
  assert.match(byId("set-hub-status").value, /hub 0\.4\.0/);
  assert.match(byId("set-hub-status").value, /schema 1/);
  assert.match(byId("set-hub-status").value, /checked 12:00:00/);
  assert.equal(byId("set-hub-fields").classList.contains("hidden"), false);
});

test("hub status renders an unreachable hub without throwing", async () => {
  await boot({
    config: baseConfig({ source_kind: "hub" }),
    hubStatus: {
      url: "http://127.0.0.1:8787",
      reachable: false,
      version: null,
      schema_version: null,
      error: "connection refused",
      checked_at: "2026-10-15 12:00:00",
    },
  });
  await tick();
  assert.match(byId("set-hub-status").value, /Unreachable: connection refused/);
});

test("hub status says so when nothing has been probed yet", async () => {
  await boot({ config: baseConfig({ source_kind: "hub" }), hubStatus: null });
  await tick();
  assert.match(byId("set-hub-status").value, /Not checked yet/);
});

test("the hub Refresh button probes on demand", async () => {
  await boot({ config: baseConfig({ source_kind: "hub" }) });
  await tick();
  byId("btn-hub-refresh").dispatchEvent(
    new global.window.MouseEvent("click", { bubbles: true }),
  );
  await tick();
  const call = [...state.calls].reverse().find((c) => c.cmd === "get_hub_status");
  assert.ok(call, "expected a live probe");
  assert.equal(call.args.url, "http://127.0.0.1:8787");
});

test("editing the hub address marks the cached status stale", async () => {
  await boot({ config: baseConfig({ source_kind: "hub" }) });
  await tick();
  const url = byId("set-hub-url");
  url.value = "http://other:8787";
  url.dispatchEvent(new global.window.Event("input"));
  assert.match(byId("set-hub-status").value, /Not checked for this address/);
});

test("saving settings never probes the hub", async () => {
  await boot({ config: baseConfig({ source_kind: "hub" }) });
  await settings.saveSettings();
  await tick();
  assert.equal(
    state.calls.some((call) => call.cmd === "get_hub_status"),
    false,
    "a save must not wait on a health probe",
  );
});

test("hub status is not probed or read for a file source", async () => {
  await boot({ config: baseConfig({ source_kind: "file" }) });
  await tick();
  assert.equal(
    state.calls.some((call) => call.cmd === "get_hub_status" || call.cmd === "get_hub_status_cached"),
    false,
    "a file source has no hub",
  );
  assert.equal(byId("set-hub-fields").classList.contains("hidden"), true);
});

test("copy diagnostics invokes the shell and toasts", async () => {
  await boot();
  byId("btn-copy-diagnostics").dispatchEvent(
    new global.window.MouseEvent("click", { bubbles: true }),
  );
  await tick();
  assert.ok(
    state.calls.some((call) => call.cmd === "copy_diagnostics"),
    "expected copy_diagnostics",
  );
  assert.equal(byId("toast").textContent, "Diagnostics copied");
});
