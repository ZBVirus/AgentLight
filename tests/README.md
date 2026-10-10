# AgentLight tests

All app-level test suites live here. Rust unit/integration tests stay in
`crates/*/` and run with `cargo test`.

| Suite | Needs | Run |
|-------|-------|-----|
| `plugin/` | Node only | `node --test tests/plugin/` |
| `frontend/` | Node + jsdom | `cd tests/frontend && npm install --include=dev && node --test` |
| `playwright/` | Node + Chromium | `cd tests/playwright && npm install --include=dev && npx playwright install chromium && npx playwright test` |
| `windows/` | Windows + WebView2 (interactive desktop for the harness) | `powershell -File tests\run-windows.ps1` |

Runners:

- `run-linux.sh` — Rust fmt/clippy/test, plugin tests, `node --check` over
  `dist/`, the jsdom frontend suite, and a live server end-to-end. Writes
  `tests/test-report/linux.txt`.
- `run-windows.ps1` — Playwright + server HTTP smoke + the native UI harness +
  the WebView2 CDP end-to-end. Writes `tests/test-report/windows.txt`.

`tests/windows/README.md` covers the sandbox setup (SDK, VC runtime, WebView2,
PsExec into Session 1, local-disk build).

CI (`.github/workflows/ci.yml`) mirrors these: `core` (Rust), `plugin`,
`frontend` (jsdom), `playwright` (Chromium), `windows-gui` (CDP end-to-end),
plus `windows-app` / `windows-server` for the release artifacts.
