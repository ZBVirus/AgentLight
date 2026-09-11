# AgentLight

An always-on-top desktop widget for Windows that mirrors
[clawlight](https://github.com/clawlight/clawlight-cli) session state as a
traffic light and a detailed session list.

clawlight watches your coding-agent sessions (opencode, Claude Code, Codex CLI,
GitHub Copilot CLI) and writes their status to a single `state.json`. AgentLight
reads that same file and shows it in a small, glanceable window: a red / orange
/ green light for the aggregate, and a per-session view with status, project,
and branch. It is aimed at the case where the agents run inside a container and
you want the status on the host desktop: bind-mount the clawlight state
directory out of the container, and AgentLight renders it.

## Status light

| Light  | Meaning                                                        |
|--------|----------------------------------------------------------------|
| Red    | At least one session needs your input (`needs_help`).          |
| Orange | No session needs input, but at least one is idle (`inactive`). |
| Green  | All live sessions are running (`active`).                      |
| Gray   | No live sessions.                                              |

This is clawlight's own aggregate rule with the default `any_inactive` yellow
mode (see `docs/state-format.md`). An optional `active_wins` mode stays green
while anything is running.

## How it reads state

- Default state file: `%USERPROFILE%\.claude\clawlight\state.json`.
- Resolution order: `AGENTLIGHT_STATE_PATH` env var, then `state_path` in the
  config file, then the default above. The window also has a "Choose file…"
  picker.
- The file is polled/watched for changes (mtime + size) and re-read on change.
- Reads never take the lock: clawlight writes atomically (temp file + rename),
  so a reader always sees a complete snapshot.

See [`docs/state-format.md`](docs/state-format.md) for the full, verified
contract, including the `.state.lock` write mutex.

## Removing sessions

The detail view can remove a session you no longer care about, mirroring the
clawlight TUI's `x`. AgentLight performs the same read-modify-write:

1. Best-effort exclusive lock on `.state.lock` beside `state.json`.
2. Read `state.json`.
3. Delete the session key, preserving every other field untouched.
4. Write a sibling temp file and atomically `rename` it over `state.json`.
5. Never write if the file could not be parsed.

The lock is advisory and OS-level, so it may not hold across a Docker bind mount
or network filesystem. AgentLight therefore proceeds unlocked if the lock cannot
be taken, exactly like clawlight. Removal only happens on an explicit user
action, which keeps the race window tiny.

## Stack

- **Rust + [Tauri](https://tauri.app/) v2**, built on Windows.
- Frontend: vanilla HTML/CSS/JS (no Node bundler); Tauri serves a static
  `dist/` directory.
- System WebView2 renders the UI, so the binary stays small and there is no
  bundled browser engine.
- Rust crates: `serde`/`serde_json` (state parsing), `notify` (file watching),
  `fs4` (the `.state.lock`, same crate clawlight uses), `chrono` (timestamps).

The state model is a straight port of clawlight's `src/state.rs` and
`src/session.rs`, so the two disagree only if the upstream contract changes.

## Planned layout

```
src-tauri/          Rust backend
  src/
    state.rs        parse state.json, aggregate, clear_session, lock
    session.rs      merge + name priority + sort + retention
    config.rs       config file + state-path resolution
    lib.rs          Tauri commands, file watcher, state-changed events
  tauri.conf.json
dist/               static frontend (index.html, style.css, app.js)
docs/               state-format.md (external contract)
tests/              parser/aggregate/clear fixtures
```

## Build & run (Windows host)

Prerequisites: Rust (rustup), the MSVC C++ build tools, and the Tauri CLI
(`cargo install tauri-cli`). WebView2 is preinstalled on Windows 10/11; if
missing, install the Evergreen runtime. The GUI is built and run on Windows;
the state/parser crate is unit-testable on any platform.

```powershell
cargo test            # parser / aggregate / clear logic
cargo tauri dev       # run the widget
cargo tauri build     # produce an installer / exe
```

Vendored or container-side Linux cross-compilation of the Tauri app is not
supported; build the GUI on Windows.

## Configuration

Config file: `%APPDATA%\AgentLight\config.json` (alongside the Tauri
identifier). Fields:

| Field            | Default            | Meaning                                            |
|------------------|--------------------|----------------------------------------------------|
| `state_path`     | *(resolved)*       | Absolute path to `state.json`.                     |
| `always_on_top`  | `true`             | Keep the window above other windows.               |
| `yellow_mode`    | `"any_inactive"`   | `any_inactive` or `active_wins` (see above).       |
| `poll_ms`        | `1500`             | Fallback poll interval when watching is unavailable. |
| `show_done`      | `false`            | Show all `done` sessions instead of the newest 5.  |
| `window`         | `{width,height,x,y}` | Window geometry, persisted on move/resize.       |

Environment override: `AGENTLIGHT_STATE_PATH`.

## Pointing the container at a host folder

AgentLight only needs to read a file, so any channel works. For opencode inside
Docker, bind-mount clawlight's state directory out to the host (read-write, so
Remove works):

```powershell
docker run ... -v %USERPROFILE%\clawlight:/home/opencode/.claude/clawlight ...
```

Then set `state_path` to `%USERPROFILE%\clawlight\state.json` (or pick it in the
app). If you prefer a read-only mount, the light and detail views still work and
only Remove is disabled.

## Limitations

- **No process reaping from the host.** clawlight marks a session `done` when the
  recorded `owner_pid` is dead. That PID belongs to the container's PID
  namespace, so a host-side reader cannot check it. AgentLight falls back to the
  same 24h staleness rule and otherwise trusts the file. A crashed session may
  stay `active` until clawlight itself writes `ended`/`done`.
- **Advisory lock across mounts.** See the caveat under Removing sessions.
- **`done` retention.** Like clawlight, the detail view keeps the newest 5
  `done` sessions unless `show_done` is set.

## Credits

AgentLight is an independent reader of the state file written by
[clawlight](https://github.com/clawlight/clawlight-cli) (MIT). It embeds no
clawlight code, but its status model and palette follow that project.
