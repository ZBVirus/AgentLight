// End-to-end test of the real AgentLight Tauri app via WebView2 remote debugging.
//
// Prereq: the app is running with additionalBrowserArgs containing
// `--remote-debugging-port=<port>` (see tests/windows/README.md); run-cdp.ps1
// starts it. The CDP port comes from AGENTLIGHT_CDP_PORT (default 9222).
const { chromium } = require("@playwright/test");

const PORT = Number(process.env.AGENTLIGHT_CDP_PORT || 9222);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
function check(name, ok, detail = "") {
  results.push({ name, ok: !!ok, detail });
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}${detail ? "  (" + detail + ")" : ""}`);
}

(async () => {
  const browser = await chromium.connectOverCDP(`http://127.0.0.1:${PORT}`);
  const context = browser.contexts()[0];
  const page = context.pages()[0];

  const innerSize = () =>
    page.evaluate(async () => {
      const win = window.__TAURI__.window.getCurrentWindow();
      const size = await win.innerSize();
      const scale = await win.scaleFactor();
      return { w: Math.round(size.width / scale), h: Math.round(size.height / scale), scale };
    });
  const winProp = (method) =>
    page.evaluate(async (m) => window.__TAURI__.window.getCurrentWindow()[m](), method);
  const view = () => page.evaluate(() => document.body.dataset.view);

  await page.reload({ waitUntil: "domcontentloaded" });
  await page.waitForLoadState("domcontentloaded");
  await sleep(2000);

  check("page is the real tauri app", page.url().includes("tauri.localhost"), page.url());
  check("window title is AgentLight", (await winProp("title")) === "AgentLight", await winProp("title"));
  check("initial view is mini", (await view()) === "mini");
  check("collapse style is single", (await page.evaluate(() => document.body.dataset.collapse)) === "single");

  const lightW = await page.locator(".view.mini .light").evaluate((el) => getComputedStyle(el).width);
  const coreH = await page.locator(".view.mini .light .light-core").evaluate((el) => getComputedStyle(el).height);
  check("collapsed single light is 62px", lightW === "62px", lightW);
  check("collapsed single core is 40px", coreH === "40px", coreH);
  check("collapsed window is not resizable", (await winProp("isResizable")) === false);
  check("native window is 88x88", JSON.stringify(await innerSize()).includes('"w":88'), JSON.stringify(await innerSize()));

  // Expand: the mini view expands on a click (drag.js release handler).
  await page.locator("#view-mini").click({ position: { x: 20, y: 20 } });
  await sleep(1800);
  check("click expands to detail", (await view()) === "detail");
  check("#view-detail is visible", await page.locator("#view-detail").isVisible());
  const detail = await innerSize();
  check("native window resized to 420x548", detail.w === 420 && detail.h === 548, JSON.stringify(detail));
  check("detail window is not resizable", (await winProp("isResizable")) === false);

  // Pin toggles the real always-on-top flag and the button state.
  const onTopBefore = await winProp("isAlwaysOnTop");
  const pinBefore = await page.locator("#btn-pin").getAttribute("class");
  await page.locator("#btn-pin").click();
  await sleep(1000);
  const onTopAfter = await winProp("isAlwaysOnTop");
  const pinAfter = await page.locator("#btn-pin").getAttribute("class");
  check("always-on-top toggles", onTopBefore !== onTopAfter, `${onTopBefore} -> ${onTopAfter}`);
  check("pin button state toggles", pinBefore !== pinAfter, `${pinBefore} -> ${pinAfter}`);

  // Settings: rows gate on the notifications flag.
  await page.locator("#btn-settings").click();
  await sleep(1000);
  check("settings view opens", await page.locator("#view-settings").isVisible());
  const version = await page.locator("#app-version").inputValue();
  check("settings shows a version", /^\d+\.\d+\.\d+/.test(version), version);
  const notifEnabled = await page.locator("#set-notifications").isChecked();
  const rowsHidden = await page.locator("#set-notification-rows").evaluate((el) => el.classList.contains("hidden"));
  check("notification rows gated by the flag", rowsHidden === !notifEnabled, `enabled=${notifEnabled} hidden=${rowsHidden}`);

  // Esc discards an edit; Ctrl+S saves and leaves. Run before changing other
  // settings so Ctrl+S persists nothing new on the test host.
  const doneBefore = await page.locator("#set-show-done").isChecked();
  await page.locator("#set-show-done").setChecked(!doneBefore);
  await page.keyboard.press("Escape");
  await sleep(600);
  check("Esc leaves Settings", (await view()) === "detail");
  await page.locator("#btn-settings").click();
  await sleep(600);
  check(
    "Esc did not save the edit",
    (await page.locator("#set-show-done").isChecked()) === doneBefore,
  );
  const saveStart = Date.now();
  await page.keyboard.press("Control+s");
  await page.waitForFunction(() => document.body.dataset.view === "detail", null, {
    timeout: 3000,
  });
  const saveMs = Date.now() - saveStart;
  check("Ctrl+S saves and leaves Settings promptly", saveMs < 1000, `${saveMs}ms`);

  // Reopen Settings for the remaining checks.
  await page.locator("#btn-settings").click();
  await sleep(600);

  // New features' settings surface.
  check(
    "session link offers OpenChamber",
    (await page.locator('#set-session-link option[value="openchamber"]').count()) === 1,
  );
  check("show-subagents toggle exists", (await page.locator("#set-show-subagents").count()) === 1);

  // "Any status change" is a select-all. Notifications must be on so the row is
  // interactive.
  await page.locator("#set-notifications").setChecked(true);
  await sleep(300);
  await page.locator("#set-notification-any").setChecked(true);
  const allTriggers = await page.evaluate(() =>
    ["set-notification-needs-help", "set-notification-done", "set-notification-subagent"].every(
      (id) => document.getElementById(id).checked,
    ),
  );
  check("any-status selects the specific triggers", allTriggers);
  await page.locator("#set-notification-done").setChecked(false);
  check(
    "unchecking a specific clears any-status",
    (await page.locator("#set-notification-any").isChecked()) === false,
  );

  // Alarm volume + Test control.
  await page.locator("#set-alarms-enabled").setChecked(true);
  await sleep(300);
  check("alarm volume slider present", (await page.locator("#set-alarm-volume").count()) === 1);
  check("alarm Test button present", (await page.locator("#btn-alarm-test").count()) === 1);

  // Each session-link mode remembers its own server address.
  await page.locator("#set-session-link").selectOption("opencode_v1");
  await page.locator("#set-session-link-base").fill("http://v1.local:4096");
  await page.locator("#set-session-link").selectOption("opencode_v2");
  const v2Value = await page.locator("#set-session-link-base").inputValue();
  check("switching link mode swaps the server address", v2Value === "", v2Value);
  await page.locator("#set-session-link").selectOption("opencode_v1");
  const v1Back = await page.locator("#set-session-link-base").inputValue();
  check("the v1 address is remembered", v1Back === "http://v1.local:4096", v1Back);
  await page.locator("#set-session-link").selectOption("producer");
  check(
    "producer mode explains where the URL comes from",
    await page.locator("#set-session-link-producer-hint").isVisible(),
  );
  check(
    "producer mode hides the base field",
    await page.locator("#set-session-link-base-field").isHidden(),
  );

  // Hub status row: probes the configured hub and reports reachability.
  // Switch to hub mode first so the (otherwise hidden) hub fields are usable.
  await page.locator("#set-source-kind").selectOption("hub");
  const hubUrlBefore = await page.locator("#set-hub-url").inputValue();
  await page.locator("#set-hub-url").fill("http://127.0.0.1:1");
  await page.locator("#btn-hub-refresh").click();
  await page.waitForFunction(
    () => /Unreachable/.test(document.getElementById("set-hub-status").value),
    null,
    { timeout: 10000 },
  );
  const hubStatus = await page.locator("#set-hub-status").inputValue();
  check("hub status reports an unreachable hub", /Unreachable/.test(hubStatus), hubStatus);
  await page.locator("#set-hub-url").fill(hubUrlBefore);
  await page.locator("#set-source-kind").selectOption("file");

  // Copy diagnostics runs through the shell (the Rust side builds the report
  // and writes the clipboard; a failure still toasts).
  check(
    "copy diagnostics button exists",
    (await page.locator("#btn-copy-diagnostics").count()) === 1,
  );
  await page.locator("#btn-copy-diagnostics").click();
  const copyStart = Date.now();
  await page.waitForFunction(
    () => /Diagnostics copied|Copy failed/.test(document.getElementById("toast").textContent || ""),
    null,
    { timeout: 3000 },
  );
  const copyMs = Date.now() - copyStart;
  const copyToast = (await page.locator("#toast").textContent()) || "";
  check(
    "copy diagnostics round-trips through the shell promptly",
    /Diagnostics copied|Copy failed/.test(copyToast) && copyMs < 1000,
    `${copyToast} in ${copyMs}ms`,
  );

  // Back leaves Settings without saving.
  await page.locator("#btn-settings-back").click();
  await sleep(800);
  check("Back returns to the detail view", (await view()) === "detail");

  // Collapse.
  await page.locator("#btn-collapse").click();
  await sleep(1500);
  check("collapse returns to mini", (await view()) === "mini");
  const mini = await innerSize();
  check("native window back to 88x88", mini.w === 88 && mini.h === 88, JSON.stringify(mini));

  // Right-click the collapsed view opens the native layout menu. The menu is an
  // OS popup, not part of the DOM, so assert the handler runs cleanly and the
  // app stays in the mini view.
  await page.locator("#view-mini").click({ button: "right", position: { x: 20, y: 20 } });
  await sleep(500);
  check("right-click on the collapsed view is handled", (await view()) === "mini");

  const failed = results.filter((r) => !r.ok).length;
  console.log(`\n${results.length - failed}/${results.length} passed`);
  await browser.close();
  process.exit(failed ? 1 : 0);
})().catch((e) => {
  console.error("ERROR:", e && e.stack ? e.stack : e);
  process.exit(2);
});
