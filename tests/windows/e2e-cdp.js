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

  // Back to the detail view, then collapse.
  await page.locator("#btn-settings-back").click();
  await sleep(1000);
  await page.locator("#btn-collapse").click();
  await sleep(1500);
  check("collapse returns to mini", (await view()) === "mini");
  const mini = await innerSize();
  check("native window back to 88x88", mini.w === 88 && mini.h === 88, JSON.stringify(mini));

  const failed = results.filter((r) => !r.ok).length;
  console.log(`\n${results.length - failed}/${results.length} passed`);
  await browser.close();
  process.exit(failed ? 1 : 0);
})().catch((e) => {
  console.error("ERROR:", e && e.stack ? e.stack : e);
  process.exit(2);
});
