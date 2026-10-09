# Project status

Living snapshot of the branch/PR/release state and what is verified. Prefer this
over git archaeology; update it whenever the branch layout changes.

_Last updated: 2026-10-09, `chore/local-tooling` @ `b378917` (the hands-on
testing rounds 2-5 are committed; **PR #4** into `feat/parked-producer`). The
native GUI was also verified end-to-end in the Windows Sandbox (Session 1):
window behavior via `tests/windows/ui-harness.ps1`, and real-WebView2 interactions via
Playwright over CDP (14/14). Backup branch `backup/2026-10-09`. See
`local/HANDOFF.md` for environment facts and commands._

## Released

- `main` = `834f7e4`, tagged **`v0.4.0`** (NSIS installer + portable exe +
  `agentlight-server` on the release page). Everything since is **unreleased**.
- `develop` = `40ca27b` (durable push store, heartbeat, SSE client groundwork).
- Older tags: `v0.3.0`, `v0.2.0-single-light`, `v0.2.0-triple-light`,
  `checkpoint-2026-09-13` (= `develop`).

## In-flight (not merged)

A stack of branches, each PR targeting the branch below it. **Do not merge
before the one below it.** None are merged.

| Branch | PR | Base |
|--------|----|------|
| `feat/sse-client` | *(none yet)* | `develop` |
| `feat/parked-fixes` | #1 | `feat/sse-client` |
| `feat/parked-window` | #2 | `feat/parked-fixes` |
| `feat/parked-producer` | #3 | `feat/parked-window` |
| `chore/local-tooling` | #4 | `feat/parked-producer` |

What each adds:

- `feat/sse-client` — initial SSE frame + `HubSource` live stream with poll
  fallback; CI runs on PRs into any base.
- `feat/parked-fixes` (PR #1) — same-monitor expand/collapse, right-click Hide,
  plugin `AGENTLIGHT_AUTOSTART_BIN` hub autostart.
- `feat/parked-window` (PR #2) — collapsed color/label customization, collapsed
  resizing (since reverted), `topmost_reassert` (since removed).
- `feat/parked-producer` (PR #3) — session deep link (`url` + `open_url`), alarm
  sounds/triggers.
- `chore/local-tooling` (current HEAD) — fixes from hands-on testing, on top of
  the whole stack:
  - **Producer-scoped snapshots** so two producers sharing a hub stop wiping each
    other's sessions (the "sessions clear then reappear" bug). `SessionEvent`
    gained `producer`; the plugin sends a stable id and never posts an empty
    snapshot. See `docs/protocol.md` and `docs/plugin.md`.
  - UI/window fixes: colors apply in both views, gray option removed, "Reset
    colors", notification triggers (`notification_trigger`), alarm rows hidden
    when disabled, `mini_show_labels` defaults off.
  - Second hands-on round:
    - **Removed-session tombstones** in `EventPushSource`: a producer heartbeat
      can no longer resurrect a session removed with the X button or "Clear
      done"; a real upsert event clears the tombstone. Persisted with the push
      store. See `docs/protocol.md`.
    - "Notify when" rows hide unless Desktop notifications is on, with an
      installed-build caveat (Windows toasts need the NSIS install, not the
      portable exe).
    - Alarm uses the Win32 sound API with a bundled two-note chime default;
      no PowerShell and no console window flash.
    - Frontend: "Choose state file…" hides for hub/push sources, Settings opens
      scrolled to the top, and an unchanged save no longer writes the resolved
      file path over a hub config (which restarted the hub source).
    - Collapsed window is fixed-size again (the aspect-snap resize fought the
      user); `mini_width` / `mini_height` and the resize persister are removed.
    - Server logs a warning when bound beyond loopback with no admin token.
    - Plugin logs its version and settings on startup so a stale copied file is
      visible; `docs/plugin.md` corrects the same-directory producer caveat and
      documents the non-local URL template.
  - Third round (no-legacy compatibility):
    - **Removed the legacy global prune**: a `mode:"snapshot"` batch without a
      `producer` now only upserts, so an old or hand-rolled producer can never
      wipe another producer's live set. See `docs/protocol.md`.
    - **Notification identity**: the app registers its `AppUserModelID` under
      `HKCU\Software\Classes\AppUserModelId` at startup and sets the process
      AUMID, so the portable exe can show Windows toasts instead of being
      silently dropped. Settings hint adjusted.
  - Fourth round (post-test UI feedback):
    - **Collapsed sizes restored**: the `vmin`-scaled lights/labels were too
      small, so the fixed 42 px / 24 px lights and 10 px labels are back at the
      fixed collapsed window sizes.
    - **`topmost_reassert` removed**: it did not hold over a Microsoft Store
      windowed-fullscreen window. The pin / "Always on top" setting is now the
      single authority on topmost; the idea is recorded as a future feature in
      `docs/ROADMAP.md`.
    - **No console flashes**: opening a session URL now uses the Win32
      `ShellExecuteW` call instead of `cmd /C start`; nothing the app does
      spawns a console window.
    - **Multi-select triggers**: desktop notifications and alarms each fire on
      any combination of `needs_help` / `done` / `any_status` (checkboxes).
    - **Server URL template**: the standalone hub accepts
      `AGENTLIGHT_SESSION_URL_TEMPLATE` and rewrites ingested session URLs, so
      it can be set where the server is launched.
    - **Toast identity**: the AppUserModelID registration writes both
      `DisplayName` and `IconUri` so the toast header reads "AgentLight".
  - Fifth round (test coverage):
    - **Web client pairing fixed**: `client.html` probes `/healthz` and shows
      state directly when the server is open (`auth: "none"`); a saved token the
      server rejects with `401` returns to pairing. See `docs/protocol.md`.
    - **`tests/frontend`**: a jsdom suite that runs on any Linux/CI host with no
      browser — collapsed styles, multi-select triggers, row gating, the Open
      action, the web-client pairing flow, and static `style.css` size checks.
    - **`tests/playwright`**: Chromium suites for the frontend (mocked
      `__TAURI__`) and the real hub web client (spawns `agentlight-server`).
    - **CI**: `agentlight-source-events` added to the core job, plus `frontend`
      (jsdom) and `playwright` jobs.
    - **Test suites + runners**: `tests/run-linux.sh`, `tests/run-windows.ps1`,
      `tests/windows/ui-harness.ps1` (Win32 + UI Automation for native window
      behavior), and `tests/windows/e2e-cdp.js` (Playwright over WebView2 CDP),
      documented in `tests/README.md` and `tests/windows/README.md`.

## Verification matrix

| Check | Where | Status |
|-------|-------|--------|
| `cargo fmt --all --check` | Linux container | green |
| `cargo clippy -p agentlight-core -p agentlight-server -p agentlight-hub-client -p agentlight-source-events --all-targets -- -D warnings` | Linux | green |
| `cargo test` (core 72, source-events 18, server 42, hub-client 21) | Linux | green |
| `node --test tests/plugin/agentlight.test.js` (20) | Linux | green |
| `node --test tests/frontend` (jsdom: frontend + web client pairing) | Linux | green |
| `node --check` on `dist/**/*.js` | Linux | green |
| `cargo check -p agentlight` (shell, staged sysroot) | Linux | green |
| Server end-to-end (healthz, ingest, tombstone, URL template) | Linux | green |
| Playwright (frontend + hub web client) | CI `ubuntu-latest`; in-sandbox via CDP | green |
| Windows build (app + server) | GitHub Actions `windows-latest`; also built locally in the Windows Sandbox | green |
| Native window behavior (title, collapsed 88×88, not resizable, no console child, AUMID) | Windows Sandbox Session 1, `tests/windows/ui-harness.ps1` | green (2026-10-09) |
| Real-WebView2 UI (expand/collapse → 88↔420×548, pin, settings gating) | Windows Sandbox, Playwright over WebView2 CDP | green 14/14 (2026-10-09) |
| Toast actually rendered (header "AgentLight") | Windows desktop | AUMID registry entry verified; rendering still needs a human |

## Building a test exe

The app and server both build on Windows CI. To get a fresh portable pair for a
branch without opening a PR, dispatch the CI workflow on that ref:

```bash
curl -X POST -H "Authorization: Bearer $TOKEN" \
  https://api.github.com/repos/ZBVirus/AgentLight/actions/workflows/ci.yml/dispatches \
  -d '{"ref":"<branch>"}'
```

Artifacts (7-day retention): `agentlight-windows-portable` (single portable
`AgentLight-portable.exe`) and `agentlight-server-windows`. `dev-build.yml`
produces only the app artifact `agentlight-windows-dev`. In the Windows Sandbox
these land under `C:\SandboxOutput\build\`.

## Environment facts (dev container)

- `cargo` is at `$HOME/.cargo/bin` — `export PATH="$HOME/.cargo/bin:$PATH"`.
- The shell crate checks with a staged sysroot; see the command in `AGENTS.md`.
- Windows-native build/test runs in the Windows Sandbox (`ssh windows`), with
  `C:\Workspace` a read-only mirror of this tree and `C:\SandboxOutput` for
  artifacts. Never edit `C:\Workspace`.

## Producer environment variables

Read by the opencode plugin (`plugins/opencode/agentlight.js`):

| Variable | Default | Meaning |
|----------|---------|---------|
| `AGENTLIGHT_HUB_URL` | `http://127.0.0.1:8787` | Hub base URL. |
| `AGENTLIGHT_TOKEN` | *(none)* | Bearer token, if the hub requires auth. |
| `AGENTLIGHT_PRODUCER` | `opencode:<host>:<directory>` | Stable snapshot-scope id. |
| `AGENTLIGHT_HEARTBEAT_MS` | `30000` | Snapshot interval; `0` disables. |
| `AGENTLIGHT_SESSION_URL_TEMPLATE` | `http://localhost:4096/session/{id}` | Deep link; empty disables. |
| `AGENTLIGHT_AUTOSTART_BIN` | *(none)* | Spawn this hub binary if `/healthz` is down. |

Server (`agentlight-server`): `AGENTLIGHT_BIND`, `AGENTLIGHT_TOKEN`,
`AGENTLIGHT_SOURCE` (`file`/`events`), `AGENTLIGHT_EVENTS_FILE`,
`AGENTLIGHT_HEARTBEAT_MS`, `AGENTLIGHT_DEVICES_FILE`.

## Document map

- `README.md` — product, configuration table, build/test.
- `AGENTS.md` — onboarding: layout, commands, hard rules.
- `docs/state-format.md` — clawlight `state.json` contract (verified).
- `docs/protocol.md` — hub HTTP/SSE wire contract, ingest modes, producer scoping.
- `docs/plugin.md` — opencode plugin setup and `SessionEvent` schema.
- `docs/architecture-redesign.md` — engine/hub design rationale.
- `docs/ROADMAP.md` — Done / Planned / Deferred.
