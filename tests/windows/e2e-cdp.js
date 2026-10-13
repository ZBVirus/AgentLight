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
  const notifEnabled = await page.locator("#set-notifications").isChecked();
  const rowsHidden = await page.locator("#set-notification-rows").evaluate((el) => el.classList.contains("hidden"));
  check("notification rows gated by the flag", rowsHidden === !notifEnabled, `enabled=${notifEnabled} hidden=${rowsHidden}`);

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

  // Back to the detail view, then collapse.
  await page.locator("#btn-settings-back").click();
  await sleep(1000);
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
