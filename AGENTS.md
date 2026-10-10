# AGENTS.md

AgentLight is a Windows always-on-top widget (Tauri v2 + static ES modules) that
renders clawlight or hub session state as a traffic light plus a detail list. The
workspace also ships an optional HTTP hub, a hub client, and an opencode producer
plugin.

Read in this order: `README.md` (product + configuration), `docs/STATUS.md`
(current branch/PR/release state and how to build a test exe), `docs/ROADMAP.md`
(what is done vs deliberately deferred), then the contract docs you need:
`docs/state-format.md` (clawlight file), `docs/protocol.md` (hub wire),
`docs/plugin.md` (producer plugin).

## Layout

- `crates/agentlight-core/` — **all state semantics**. No GUI, no Tauri, no
  async runtime. Builds and tests on any host. Put parsing, aggregation, and
  config here, not in the shells.
  - `src/source.rs` + `src/source/clawlight.rs` — the `StateSource` trait and the
    clawlight file adapter (the compatibility source). New integrations are
    adapters here; never teach the engine or the normalized model a new file
    shape.
  - `src/engine.rs` — merge, retention, aggregate, revisions, notification and
    alarm edges. Owns source lifecycle; shells only deliver `Update`s.
  - `src/config.rs` — preferences, serde `snake_case`, `#[serde(default)]`.
  - `src/session.rs`, `src/snapshot.rs`, `src/state.rs` — display rows, the wire
    snapshot, and the clawlight parse/aggregate/lock/clear primitives.
- `crates/agentlight-source-events/` — `EventPushSource`: the in-memory push
  source used by the hub's events mode. Holds the durable store and the
  **producer-scoped snapshot** semantics (see Hard rules).
- `crates/agentlight-server/` — async (`tokio`/`axum`) hub: routes, auth,
  devices, embedded web client. Builds the standalone `agentlight-server` binary
  and can be embedded in the desktop (`AgentLight` links this crate).
- `crates/agentlight-hub-client/` — `HubClient` + `HubSource` (SSE with poll
  fallback) + the `agentlight-hook` CLI.
- `src-tauri/` — thin Tauri v2 shell: commands, tray, autostart, window
  geometry/resize, desktop toasts, alarm sound. No state semantics live here.
- `dist/` — static HTML/CSS/JS frontend. No bundler, no Node. Tauri
  `withGlobalTauri` exposes `window.__TAURI__`; all IO goes through Rust commands.
- `plugins/opencode/` — the opencode producer plugins: `agentlight.js` for the
  OpenCode V1 plugin API and `agentlight-v2.js` for the V2 API (tests in
  `tests/plugin/`). Both are event-bus observers: they only report, never change
  opencode.
- `tests/` — app-level suites: `plugin/`, `frontend/` (jsdom), `playwright/`,
  `windows/` (native harness + WebView2 CDP), plus the `run-linux.sh` /
  `run-windows.ps1` runners. See `tests/README.md`.
- `docs/` — the contract/design/status docs listed above.
- `scripts/make_icons.py`, `scripts/make_alarm.py` — regenerate the icons and the
  bundled default alarm with no deps.

## Architecture in one paragraph

The engine owns one active `StateSource`: the clawlight file adapter, the
remote-hub adapter, or the push (`EventPushSource`) adapter. The hub
(`agentlight-server`) is the async edge; it wraps a synchronous engine and
exposes `snapshot` / SSE `events` / `ingest` / `commands` over HTTP. Producers
(agents) POST `SessionEvent`s to `/api/v1/ingest`; `mode:"snapshot"` is
authoritative and prunes, and is scoped by `producer`. The desktop either reads a
local file, reads a remote hub, or hosts the embedded hub over its own engine.
`docs/architecture-redesign.md` has the full rationale.

## Commands

```bash
# Rust: Debian's system cargo/rustfmt/clippy 1.85 at /usr/bin. A rustup toolchain
# in ~/.cargo is optional and is NOT persisted when the container is recreated;
# the repo builds and lints on the system toolchain.
cargo fmt --all --check
cargo clippy -p agentlight-core -p agentlight-server -p agentlight-hub-client \
  -p agentlight-source-events --all-targets -- -D warnings
cargo test  -p agentlight-core -p agentlight-server -p agentlight-hub-client \
  -p agentlight-source-events
node --check dist/views/detail.js      # any changed dist module

cargo tauri dev                        # GUI, Windows only
cargo tauri build                      # NSIS installer
```

Every app-level suite lives under `tests/` (see `tests/README.md`):

```bash
bash tests/run-linux.sh                 # Rust + plugin + jsdom + server e2e
powershell -File tests\run-windows.ps1  # Playwright + server smoke + native harness + WebView2 CDP
```

In-container `npm install` needs `--include=dev` (`NODE_ENV=production` is set),
and Playwright's Chromium cannot launch (missing system libs): run the jsdom
suite in-container, and Playwright/CI and the Windows GUI suite elsewhere. The
Windows GUI suite (Tauri/WebView2, Session 1 + PsExec) is in
`tests/windows/README.md`; `tests/windows/build-local.ps1` provisions the sandbox
toolchain automatically (`tests/windows/setup-toolchain.ps1`), and the
`windows-sandbox` skill covers the sandbox mechanics (build on the local disk,
ephemeral vs persistent, `os error 1392`).

The GUI does **not** compile on a bare Linux box (needs `webkit2gtk`/glib); CI
builds it on `windows-latest`. `agentlight-core` must keep compiling and testing
on Linux — that is the point of the split. To type-check the shell on Linux,
stage a sysroot with the Tauri prerequisites and run:

```bash
SYSROOT="$HOME/sysroot" \
PKG_CONFIG_LIBDIR="$HOME/sysroot/usr/lib/pkgconfig:$HOME/sysroot/usr/share/pkgconfig" \
PKG_CONFIG_SYSROOT_DIR="$HOME/sysroot" PKG_CONFIG_PATH="" \
cargo check -p agentlight
```

`cargo clippy -p agentlight` fails on musl (`E0463`); `cargo check` is the Linux
validation for the shell. (`~/sysroot` is not currently staged in this container,
so `tests/run-linux.sh` skips this step; stage the Tauri prerequisites to enable
it.) The Windows-only native path (topmost re-assert) is compiled by Windows CI,
not locally.

## Hard rules

- **Status is snake_case JSON**: `active` / `inactive` / `needs_help` / `done`.
  UI labels: `working` / `paused` / `needs help` / `done`. Aggregate strings to
  the frontend: `red` / `orange` / `green` / `gray`.
- **Preserve unknown fields** on every write. `clear_session`/`clear_done` work
  on the raw `serde_json::Value`, never on the typed struct, so fields this repo
  does not model survive untouched.
- **Reads never lock.** Writes (Remove / Clear done) take the best-effort
  `.state.lock`, then temp file + atomic rename. Never write if the read failed
  to parse. Proceed unlocked if the lock cannot be taken — clawlight does.
- **Never treat `terminal.owner_pid` as a host PID.** It is namespaced to the
  container; only the 24h `last_updated` staleness rule is available to us.
  `reap_stale` must stay PID-free.
- **Truncate names by chars, not bytes.** User/LLM names are arbitrary UTF-8.
- Match clawlight's aggregate: any `needs_help` → red; else `any_inactive`
  (default) any idle → orange; else any `active` → green; none → gray.
  `active_wins` is the alternate mode.
- Keep only the newest 5 `done` sessions unless `show_done` is set.
- Keep `agentlight-core` free of `tauri`/GUI/Windows-only dependencies. It stays
  **synchronous**: no tokio or other async runtime. Async belongs to the server
  and client crates.
- **New integrations are source adapters.** Implement `StateSource` (see
  `src/source.rs`) and translate the external shape there. The clawlight file
  adapter (`ClawlightFileSource`) is the compatibility source and must keep
  behaving exactly as before.
- **A `mode:"snapshot"` ingest is producer-scoped, never global.**
  `EventPushSource` prunes only the batch producer's absent sessions; a batch
  with no `producer` only upserts, so an old or hand-rolled producer cannot wipe
  another producer's live set. A producer must never POST an empty snapshot.
  A removal (X button / Clear done) is tombstoned so a later heartbeat cannot
  resurrect it; a real upsert clears the tombstone. Wire fields are additive
  (`SessionEvent.producer`, etc.); unknown fields are ignored on read, so no
  protocol bump is needed for additions.
- **Notifications and alarms are separate edges** computed by the same
  `triggers_fire` helper, each with its own `*_trigger` list (`needs_help` /
  `done` / `any_status`; fires on the first match) and its own previous-status
  map.
- **Never commit secrets or runtime state.** The GitHub push token lives outside
  the tree; `devices.json`, `push-state.json`, and `local/` are gitignored.

## Conventions

- Rust 2021 workspace (members: the four `crates/*` plus `src-tauri`). `serde`
  snake_case for the state model; mirror clawlight type names (`Status`,
  `SessionStatus`, `HookState`, `Aggregate`) so the port stays reviewable.
- Reuse clawlight's crate choices for shared concerns: `fs4` for the lock,
  `notify` for watching, `chrono` for timestamps.
- Tauri command args are camelCase on the JS side (`sessionId` ↔ `session_id`).
- Add tests for any behavior change. `snapshot::build_snapshot_at` takes an
  injected clock for deterministic tests; `src-tauri` geometry helpers are
  pure and unit-tested. New app-level suites go under `tests/` (see
  `tests/README.md`); keep plugin tests in `tests/plugin/`.
- If clawlight's state contract changes, update `docs/state-format.md` and the
  parser together. Do not guess semantics from the JSON alone.
- If you add or change a config field, update the README table and
  `docs/ROADMAP.md`, and keep `Config` defaults in sync with `dist/views/settings.js`.
