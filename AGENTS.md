# AGENTS.md

AgentLight is a Windows always-on-top widget that reads clawlight's
`state.json` and renders a session traffic light plus a detail list. See
`README.md` for the product overview and `docs/ROADMAP.md` for what is
deliberately deferred.

## Layout

- `crates/agentlight-core/` — **all state logic**. No GUI, no Tauri. Builds and
  tests on any host. Put parsing/aggregation/config here, not in the shell.
- `src-tauri/` — thin Tauri v2 shell: commands, tray, watcher, autostart,
  notifications. No state semantics live here.
- `dist/` — static HTML/CSS/JS. No bundler, no Node. Tauri `withGlobalTauri`
  exposes `window.__TAURI__`; all IO goes through Rust commands.
- `docs/state-format.md` — the verified clawlight contract. Read it before
  touching anything state-related.
- `scripts/make_icons.py` — regenerates `src-tauri/icons/` with no deps.

## Commands

```bash
cargo test -p agentlight-core                              # state/config/snapshot tests
cargo clippy -p agentlight-core --all-targets -- -D warnings
cargo fmt --all --check
cargo tauri dev                                            # GUI, Windows only
cargo tauri build                                          # NSIS installer
```

The GUI does **not** compile on a bare Linux box (needs `webkit2gtk`/glib).
CI builds it on `windows-latest`. The core crate must keep compiling and testing
on Linux — that is the point of the split.

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
- Keep `agentlight-core` free of `tauri`/GUI/Windows-only dependencies.

## Conventions

- Rust 2021 workspace. `serde` snake_case for the state model; mirror clawlight
  type names (`Status`, `SessionStatus`, `HookState`, `Aggregate`) so the port
  stays reviewable against upstream.
- Reuse clawlight's crate choices for shared concerns: `fs4` for the lock,
  `notify` for watching, `chrono` for timestamps.
- Tauri command args are camelCase on the JS side (`sessionId` ↔ `session_id`).
- Add tests in `agentlight-core` for any behavior change; `snapshot::build_snapshot_at`
  takes an injected clock for deterministic tests.
- If clawlight's state contract changes, update `docs/state-format.md` and the
  parser together. Do not guess semantics from the JSON alone.
