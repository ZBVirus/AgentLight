# AGENTS.md

AgentLight is a Windows always-on-top widget that reads clawlight's
`state.json` and renders a session traffic light plus a detail list. See
`README.md` for the product overview.

## Status

Docs-only right now. The app is not scaffolded yet. Planned stack: Rust +
Tauri v2, vanilla HTML/CSS/JS frontend, built on Windows. Do not assume source
files or a `Cargo.toml` exist until they do.

## Read this before touching state

- `docs/state-format.md` is the verified clawlight state contract (v0.13.0),
  with upstream line references. Treat it as the source of truth for field
  names, status values, aggregation, locking, and reaping.
- If you change anything about state parsing, re-check upstream
  `clawlight/clawlight-cli` (`src/state.rs`, `src/session.rs`, `src/hook.rs`).
  Do not guess semantics from the JSON alone.

## Hard rules

- **Status is snake_case JSON**: `active` / `inactive` / `needs_help` / `done`.
  UI labels are `working` / `paused` / `needs help` / `done`.
- **Preserve unknown fields** on any write. Parse permissively; a missing
  optional field must not crash.
- **Reads never lock.** clawlight writes atomically (temp + rename), so readers
  always see a complete snapshot.
- **Writes (Remove)**: best-effort exclusive lock on `.state.lock`, then
  read-modify-write, then temp file + atomic rename. Never write if the read
  failed to parse. Proceed unlocked if the lock cannot be taken — clawlight does
  the same.
- **Never treat `terminal.owner_pid` as a host PID.** It is namespaced to the
  container. Use only the 24h `last_updated` staleness rule when a PID cannot be
  validated.
- **Truncate names by chars, not bytes.** User/LLM names are arbitrary UTF-8;
  byte slices panic.
- Match clawlight's aggregate: any `needs_help` → red; else `any_inactive`
  (default) any idle → orange; else any `active` → green; none → gray.
- Keep only the newest 5 `done` sessions unless configured otherwise.

## Defaults

- State path: `%USERPROFILE%\.claude\clawlight\state.json`, overridden by
  `AGENTLIGHT_STATE_PATH`, then the config `state_path`.
- Config: `%APPDATA%\AgentLight\config.json`.
- Palette (from clawlight `ui.rs`): green `#98c379`, yellow `#e5c07b`, red
  `#e06c75`, cyan `#56b6c2`, dim `#5c6370`.

## Commands

Target commands once scaffolded (verify against `Cargo.toml` / `tauri.conf.json`
before relying on them):

```powershell
cargo test            # parser / aggregate / clear logic
cargo tauri dev       # run the widget (Windows)
cargo tauri build     # package
```

The GUI builds and runs on Windows only. The state/parser crate must stay
platform-independent and unit-tested so it can run on any host.

## Repo conventions

- Rust, `serde` with `rename_all = "snake_case"` for the state model; mirror
  clawlight's type names (`Status`, `SessionStatus`, `HookState`, `Aggregate`)
  so the port stays reviewable against upstream.
- Reuse clawlight's crate choices for the shared concerns: `fs4` for the lock,
  `notify` for watching, `chrono` for timestamps.
- Frontend is static files, no Node bundler.
- Put non-obvious state/session details in `docs/state-format.md`, not in code
  comments.
