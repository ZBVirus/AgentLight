const { test, expect } = require("@playwright/test");

const NOW = new Date().toISOString();

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
    start_at_login: false,
    server_enabled: false,
    server_bind: "127.0.0.1:8787",
    server_token_hash: null,
    ...overrides,
  };
}

function baseSnapshot(overrides = {}) {
  return {
    ok: true,
    error: null,
    state_path: "C:\\Users\\me\\.claude\\clawlight\\state.json",
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
        last_updated: NOW,
        is_done: false,
        url: "http://opencode-home/session/s1",
      },
    ],
    yellow_mode: "any_inactive",
    generated_at: NOW,
    ...overrides,
  };
}

async function boot(page, { config = baseConfig(), snapshot = baseSnapshot() } = {}) {
  await page.addInitScript(
    ({ config, snapshot }) => {
      const calls = [];
      window.__calls = calls;
      window.__TAURI__ = {
        core: {
          invoke: async (cmd, args) => {
            calls.push({ cmd, args });
            if (cmd === "get_config") return config;
            if (cmd === "get_snapshot") return snapshot;
            if (cmd === "set_config") return config;
            if (cmd === "get_server_status")
              return {
                enabled: false,
                url: null,
                admin_token_set: false,
                pairing_code: null,
                pairing_expires_at: null,
                devices: [],
              };
            if (cmd === "clear_session") return true;
            if (cmd === "clear_done") return 0;
            return null;
          },
        },
        event: { listen: async () => () => {} },
        window: { getCurrentWindow: () => ({ startDragging: async () => {} }) },
      };
    },
    { config, snapshot },
  );
  await page.goto("/index.html");
  await expect(page.locator("#agg-label")).not.toHaveText("Waiting for clawlight…");
}

async function goTo(page, view) {
  await page.evaluate(async (v) => {
    const store = await import("/lib/store.js");
    store.setView(v);
  }, view);
}

test("collapsed single view keeps the old fixed light sizes", async ({ page }) => {
  await boot(page);
  expect(await page.evaluate(() => document.body.dataset.view)).toBe("mini");
  expect(await page.evaluate(() => document.body.dataset.collapse)).toBe("single");

  const light = await page.locator(".view.mini .light").evaluate((el) => getComputedStyle(el).width);
  const core = await page.locator(".view.mini .light .light-core").evaluate((el) => getComputedStyle(el).height);
  expect(light).toBe("62px");
  expect(core).toBe("40px");

  await expect(page.locator("#mini-single")).toBeVisible();
  await expect(page.locator("#mini-triple")).toBeHidden();
});

test("collapsed triple lights keep the old fixed sizes", async ({ page }) => {
  await boot(page, { config: baseConfig({ collapse_style: "triple" }) });
  expect(await page.evaluate(() => document.body.dataset.collapse)).toBe("triple");

  await expect(page.locator("#mini-triple")).toBeVisible();
  await expect(page.locator("#mini-single")).toBeHidden();

  const light = await page.locator("#mini-red").evaluate((el) => getComputedStyle(el).width);
  const core = await page.locator("#mini-red .core").evaluate((el) => getComputedStyle(el).height);
  const label = await page.locator("#mini-triple .mini-label").first().evaluate((el) => getComputedStyle(el).fontSize);
  expect(light).toBe("42px");
  expect(core).toBe("24px");
  expect(label).toBe("10px");
});

test("settings opens scrolled to the top", async ({ page }) => {
  await boot(page);
  await goTo(page, "detail");
  await page.click("#btn-settings");
  await expect(page.locator("#view-settings")).toBeVisible();

  // Scroll the form down, leave, and come back: it must reset.
  await page.evaluate(() => {
    const form = document.getElementById("settings-form");
    form.scrollTop = 120;
  });
  await page.click("#btn-settings-back");
  await page.click("#btn-settings");
  expect(await page.evaluate(() => document.getElementById("settings-form").scrollTop)).toBe(0);
});

test("triggers populate as a multi-select", async ({ page }) => {
  await boot(page);
  await goTo(page, "detail");
  await page.click("#btn-settings");

  await expect(page.locator("#set-notification-needs-help")).toBeChecked();
  await expect(page.locator("#set-notification-done")).toBeChecked();
  await expect(page.locator("#set-notification-any")).not.toBeChecked();

  // Empty alarm_trigger means nothing selected.
  await expect(page.locator("#set-alarm-needs-help")).not.toBeChecked();
  await expect(page.locator("#set-alarm-done")).not.toBeChecked();
  await expect(page.locator("#set-alarm-any")).not.toBeChecked();
});

test("saving collects every selected trigger", async ({ page }) => {
  await boot(page, { config: baseConfig({ alarms_enabled: true }) });
  await goTo(page, "detail");
  await page.click("#btn-settings");
  await page.check("#set-notification-any");
  await page.check("#set-alarm-needs-help");
  await page.click("#btn-save");

  const saved = await page.evaluate(() => {
    const call = [...window.__calls].reverse().find((c) => c.cmd === "set_config");
    return call ? call.args.config : null;
  });
  expect(saved).not.toBeNull();
  expect(saved.notification_trigger).toEqual(["needs_help", "done", "any_status"]);
  expect(saved.alarm_trigger).toEqual(["needs_help"]);
});

test("notification and alarm rows hide while disabled", async ({ page }) => {
  await boot(page, { config: baseConfig({ notifications: false, alarms_enabled: false }) });
  await goTo(page, "detail");
  await page.click("#btn-settings");
  await expect(page.locator("#set-notification-rows")).toBeHidden();
  await expect(page.locator("#set-alarm-rows")).toBeHidden();
});

test("notification and alarm rows show while enabled", async ({ page }) => {
  await boot(page, { config: baseConfig({ notifications: true, alarms_enabled: true }) });
  await goTo(page, "detail");
  await page.click("#btn-settings");
  await expect(page.locator("#set-notification-rows")).toBeVisible();
  await expect(page.locator("#set-alarm-rows")).toBeVisible();
});

test("choose-state-file is hidden for a hub source", async ({ page }) => {
  await boot(page, {
    snapshot: baseSnapshot({ ok: false, exists: false, source_kind: "hub", error: "Waiting for the hub" }),
  });
  await expect(page.locator("#btn-pick")).toHaveClass(/hidden/);
});

test("choose-state-file is shown for a file source", async ({ page }) => {
  await boot(page, {
    config: baseConfig({ source_kind: "file" }),
    snapshot: baseSnapshot({
      ok: false,
      exists: false,
      source_kind: "file",
      source_label: "C:\\state.json",
      error: "Waiting for clawlight state file",
    }),
  });
  await expect(page.locator("#btn-pick")).not.toHaveClass(/hidden/);
});

test("detail rows render and show Open for a session with a url", async ({ page }) => {
  await boot(page);
  await goTo(page, "detail");
  await expect(page.locator("#sessions li.session")).toHaveCount(1);
  await expect(page.locator("#sessions .name")).toHaveText("Fix auth");
  const open = page.locator("#sessions .open");
  await expect(open).toBeVisible();
  await expect(open).toHaveText("Open");
  await expect(open).toHaveAttribute("title", "http://opencode-home/session/s1");
});

test("a session without a url has no Open button", async ({ page }) => {
  const snap = baseSnapshot();
  snap.sessions[0].url = null;
  await boot(page, { snapshot: snap });
  await goTo(page, "detail");
  await expect(page.locator("#sessions li.session")).toHaveCount(1);
  await expect(page.locator("#sessions .open")).toHaveCount(0);
});

test("Open invokes the native open_url command", async ({ page }) => {
  await boot(page);
  await goTo(page, "detail");
  await page.click("#sessions .open");
  const call = await page.evaluate(() =>
    [...window.__calls].reverse().find((c) => c.cmd === "open_url"),
  );
  expect(call).toBeTruthy();
  expect(call.args.url).toBe("http://opencode-home/session/s1");
});
