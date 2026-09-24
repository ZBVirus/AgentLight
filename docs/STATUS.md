# Project status

Living snapshot of the branch/PR/release state and what is verified. Prefer this
over git archaeology; update it whenever the branch layout changes.

_Last updated: 2026-09-24, `chore/local-tooling` @ `edb0de0`._

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
| `chore/local-tooling` | *(none yet)* | `feat/parked-producer` |

What each adds:

- `feat/sse-client` — initial SSE frame + `HubSource` live stream with poll
  fallback; CI runs on PRs into any base.
- `feat/parked-fixes` (PR #1) — same-monitor expand/collapse, right-click Hide,
  plugin `AGENTLIGHT_AUTOSTART_BIN` hub autostart.
- `feat/parked-window` (PR #2) — collapsed color/label customization, collapsed
  resizing, `topmost_reassert`.
- `feat/parked-producer` (PR #3) — session deep link (`url` + `open_url`), alarm
  sounds/triggers.
- `chore/local-tooling` (current HEAD) — fixes from hands-on testing, on top of
  the whole stack:
  - **Producer-scoped snapshots** so two producers sharing a hub stop wiping each
    other's sessions (the "sessions clear then reappear" bug). `SessionEvent`
    gained `producer`; the plugin sends a stable id and never posts an empty
    snapshot. See `docs/protocol.md` and `docs/plugin.md`.
  - UI/window fixes: colors apply in both views, gray option removed, "Reset
    colors", collapsed lights/labels scale, diagonal aspect-locked collapsed
    resize with a per-layout minimum, topmost toggle exposed, native
    `SetWindowPos` re-assert, notification triggers (`notification_trigger`),
    alarm rows hidden when disabled, `mini_show_labels` defaults off.

## Verification matrix

| Check | Where | Status |
|-------|-------|--------|
| `cargo fmt --all --check` | Linux container | green |
| `cargo clippy -p agentlight-core -p agentlight-server -p agentlight-hub-client -p agentlight-source-events --all-targets -- -D warnings` | Linux | green |
| `cargo test` (core 70, source-events 13, server 39, hub-client 21) | Linux | green |
| `node --test plugins/opencode/test/agentlight.test.js` (20) | Linux | green |
| `node --check` on `dist/**/*.js` | Linux | green |
| `cargo check -p agentlight` (shell, staged sysroot) | Linux | green |
| Windows build (app + server) | GitHub Actions `windows-latest` | green |
| Window behavior, sounds, topmost over fullscreen, drag/resize | Windows desktop | **not verified — needs a human** |

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
