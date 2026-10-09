# AgentLight Playwright tests

Headless Chromium, two suites:

- `tests/ui.spec.js` — the static `dist/` frontend with a mocked
  `window.__TAURI__`: collapsed single/triple light sizes, settings opens at the
  top, multi-select notification/alarm triggers, notify/alarm rows hidden while
  disabled, "choose state file" gated by source kind, detail rows, and the Open
  action invoking `open_url`.
- `tests/webui.spec.js` — the real hub web client (`client.html`) against the
  built `agentlight-server`: `/healthz`, an open server skipping the pairing
  form, ingest, render, the Open link, and a live SSE update. It starts the
  server itself; the binary is found via `AGENTLIGHT_SERVER`, else `target\ci`,
  `target\release`, `target\debug`, then `C:\SandboxOutput\build`.

**Limits:** Playwright cannot drive the Tauri/WebView2 desktop window. Resize,
always-on-top, tray, and the real collapsed window size are covered by the
Windows UI Automation harness (`tests/windows/ui-harness.ps1`, needs an interactive
desktop).

## Setup / run

```bash
npm install --include=dev
npx playwright install chromium     # add --with-deps on Linux CI
npx playwright test
```

The `frontend` suite in `../frontend` (jsdom) covers the same DOM behaviour with
no browser, so it also runs in minimal Linux environments.
