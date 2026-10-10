# Project status

Living snapshot of the branch/PR/release state and what is verified. Prefer this
over git archaeology; update it whenever the branch layout changes.

_Last updated: 2026-10-09 (cleanup), `develop` @ `5bebedf` (the hands-on
testing rounds 2-5 and the whole parked stack are now integrated here). The
native GUI was also verified end-to-end in the Windows Sandbox (Session 1):
window behavior via `tests/windows/ui-harness.ps1`, and real-WebView2 interactions via
Playwright over CDP (14/14). The former in-flight and `backup/*` branches were
consolidated/deleted; every pre-cleanup commit is retained by the
`archive/pre-cleanup-2026-10-09` tag and the snapshot bundle. See the "Branch
layout" and "Cleanup" sections below, and `local/HANDOFF.md` for environment
facts and commands._

## Released

- `main` = `834f7e4`, tagged **`v0.4.0`** (NSIS installer + portable exe +
  `agentlight-server` on the release page). Everything since is **unreleased**.
- `develop` = `5bebedf` (integrated v0.5 line: durable push store, heartbeat,
  SSE client, producer-scoped snapshots and tombstones, alarms and deep links,
  the frontend/Playwright test suites). Fast-forwarded from the former parked
  stack; see "Cleanup" below.
- Older tags: `v0.3.0`, `v0.2.0-single-light`, `v0.2.0-triple-light`,
  `checkpoint-2026-09-13` (= `develop`).

## Branch layout

- `main` — released line, **protected**. Only releases reach it (`v0.4.0` = `834f7e4`).
- `develop` — the single long-lived integration branch. All in-flight work is
  already contained here.
- `archive/*` tags — immutable safety refs from the 2026-10-09 cleanup.

Short-lived work branches are created off `develop` and deleted once contained;
feature branches are not kept after their commits land in `develop`.

## Cleanup (2026-10-09)

The repo carried a linear PR stack plus two `backup/*` branches. Everything in
the non-backup branches was already contained in the stack top, so the layout was
collapsed:

- The stack (`feat/sse-client` -> `feat/parked-fixes` -> `feat/parked-window` ->
  `feat/parked-producer` -> `chore/local-tooling`, PRs #1-#4) was fast-forwarded
  into `develop`; those branch refs were deleted. The PRs are superseded by
  `develop` and can be closed.
- `feat/agentlight-app` / `-toggle` / `-triple-light` / `architecture-redesign`
  were already merged into `main`; their redundant refs were deleted.
- `backup/2026-10-09` and `backup/pre-ui-revert` had one unique commit each;
  those commits are preserved as the `archive/backup-2026-10-09` and
  `archive/backup-pre-ui-revert` tags, and the branches were deleted.
- A complete pre-cleanup snapshot (all refs, full history) was written outside
  the tree as a git bundle and a bare mirror clone; see the manifest in
  `AgentLight-cleanup-snapshot-2026-10-09/`.

Nothing reachable from `main` or the old stack was lost: the whole pre-cleanup
history is reachable from `archive/pre-cleanup-2026-10-09` (`5bebedf`).

### What the integrated stack adds

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
| `cargo test` (core 89, source-events 18, server 42, hub-client 21) | Linux | green |
| `node --test tests/plugin/` (49: V1 20 + V2 29) | Linux | green |
| `node --test tests/frontend` (jsdom: frontend + web client pairing) | Linux | green |
| `node --check` on `dist/**/*.js` | Linux | green |
| `cargo check -p agentlight` (shell, staged sysroot) | Linux | green |
| Server end-to-end (healthz, ingest, tombstone, URL template) | Linux | green |
| Playwright (frontend + hub web client) | CI `ubuntu-latest` | green |
| Windows build (app + server) | GitHub Actions `windows-latest`; also built locally in the Windows Sandbox | green |
| Native window behavior (title, collapsed 88×88, not resizable, no console child, AUMID) | Windows Sandbox Session 1, `tests/windows/ui-harness.ps1` | green (2026-10-09) |
| Real-WebView2 UI (expand/collapse → 88↔420×548, pin, settings gating, not resizable) | CI `windows-latest` (`windows-gui`) and Windows Sandbox (`tests/windows/e2e-cdp.js`) | green 18/18 (2026-10-09) |
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

Read by the opencode plugins (`plugins/opencode/agentlight.js` for V1,
`plugins/opencode/agentlight-v2.js` for V2):

| Variable | Default | Meaning |
|----------|---------|---------|
| `AGENTLIGHT_HUB_URL` | `http://127.0.0.1:8787` | Hub base URL. |
| `AGENTLIGHT_TOKEN` | *(none)* | Bearer token, if the hub requires auth. |
| `AGENTLIGHT_PRODUCER` | `opencode:<host>:<directory>` | Stable snapshot-scope id. |
| `AGENTLIGHT_HEARTBEAT_MS` | `30000` | Snapshot interval; `0` disables. |
| `AGENTLIGHT_SESSION_URL_TEMPLATE` | `http://localhost:4096/session/{id}` | Deep link; empty disables. |
| `AGENTLIGHT_AUTOSTART_BIN` | *(none)* | Spawn this hub binary if `/healthz` is down. |
| `AGENTLIGHT_DEBUG` | *(off)* | `1` logs each event type and payload keys to opencode's logs. |

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
