# AgentLight

An always-on-top desktop widget for Windows that mirrors
[clawlight](https://github.com/clawlight/clawlight-cli) session state as a
traffic light and a detailed session list.

clawlight watches your coding-agent sessions (opencode, Claude Code, Codex CLI,
GitHub Copilot CLI) and writes their status to a single `state.json`. AgentLight
reads that same file and shows it in a small, glanceable window: a red / orange
/ green light for the aggregate, and a per-session view with status, project,
harness badge, and relative time. It is aimed at the case where the agents run
inside a container and you want the status on the host desktop: bind-mount the
clawlight state directory out of the container, point AgentLight at it, done.

## Status light

| Light  | Meaning                                                        |
|--------|----------------------------------------------------------------|
| Red    | At least one session needs your input (`needs_help`).          |
| Orange | No session needs input, but at least one is idle (`inactive`). |
| Green  | All live sessions are running (`active`).                      |
| Gray   | No live sessions.                                              |

This is clawlight's own aggregate rule with the default `any_inactive` yellow
mode (see [`docs/state-format.md`](docs/state-format.md)). Switch to
`active_wins` in settings to stay green while anything is running.

## Screens

- **Light** — one large light plus live counts. The default view.
- **Details** — every session: status dot, name, harness badge (`oc` / `cx` /
  `co`), project, relative time, and a remove button. A "Clear done" action
  drops all `done` rows.
- **Settings** — state path (with a file picker), always-on-top, start at login,
  notifications, show-every-done, idle behavior, and the poll interval.

The window is frameless and draggable by its top bar, and it lives in the system
tray: closing or hiding it keeps it running, and the tray icon toggles it back.
Left-clicking the tray icon shows/hides the window; the tray menu has Settings
and Quit.

## How it reads state

- Resolution order for the state file: `AGENTLIGHT_STATE_PATH` env var, then
  `state_path` in the config, then `%USERPROFILE%\.claude\clawlight\state.json`.
  If the file is missing or unreadable the window says so and offers a picker.
- The file is watched with `notify`, with a poll backstop (default 1500 ms) for
  filesystems that emit no events — for example a Windows host reading a Docker
  bind mount.
- Reads never take the lock: clawlight writes atomically (temp file + rename),
  so a reader always sees a complete snapshot.

The full, verified contract is in [`docs/state-format.md`](docs/state-format.md).

## Removing sessions

The detail view can remove a session you no longer care about, mirroring the
clawlight TUI's `x`. AgentLight performs the same read-modify-write:

1. Best-effort exclusive lock on `.state.lock` beside `state.json`.
2. Read `state.json` as a raw JSON document.
3. Delete the session key, preserving every other field — including unknown
   fields this app has never heard of.
4. Write a sibling temp file and atomically rename it over `state.json`.
5. Never write if the file could not be parsed.

The lock is advisory and OS-level, so it may not hold across a Docker bind mount
or network filesystem. AgentLight therefore proceeds unlocked if the lock cannot
be taken, exactly like clawlight, and only writes on an explicit user action.

## Architecture

```
crates/agentlight-core/   Rust library, no GUI deps, unit-tested everywhere
  src/state.rs            parse state.json, aggregate, reap, lock, clear, atomic write
  src/session.rs          display rows: names, badges, ordering, done retention
  src/config.rs           preferences + path resolution
  src/snapshot.rs         the JSON payload the frontend renders
src-tauri/                Tauri v2 shell (Windows/macOS/Linux)
  src/lib.rs              commands, tray, watcher, autostart, notifications
  tauri.conf.json         frameless transparent always-on-top window
dist/                     static HTML/CSS/JS frontend (no bundler, no Node)
docs/                     state contract, roadmap
scripts/make_icons.py     regenerates the app icons
```

The split keeps all state logic in a crate that builds and tests on any host;
`src-tauri` is a thin shell. The aggregate, staleness, and clear semantics are a
port of clawlight's own (see `docs/state-format.md` for the mapping and
upstream line references).

## Building

The library and its tests build anywhere with a Rust toolchain. The GUI needs
the Tauri v2 prerequisites for the target OS; on Windows that means the MSVC C++
build tools and WebView2 (preinstalled on Windows 10/11, otherwise the Evergreen
runtime). No Node toolchain is required — the frontend is static.

```bash
# library: parse / aggregate / clear / snapshot
cargo test -p agentlight-core
cargo clippy -p agentlight-core --all-targets -- -D warnings
cargo fmt --all --check

# GUI (Windows)
cargo install tauri-cli --version "^2.0.0" --locked
cargo tauri dev
cargo tauri build      # NSIS installer under target/release/bundle/nsis/
```

CI (`.github/workflows/ci.yml`) runs the core tests on Linux and builds the app
on Windows, uploading the raw exe and the NSIS installer as artifacts.

## Configuration

Config file: `%APPDATA%\AgentLight\config.json`. It is created on first save and
can be edited by hand.

| Field            | Default           | Meaning                                            |
|------------------|-------------------|----------------------------------------------------|
| `state_path`     | *(resolved)*      | Absolute path to `state.json`.                     |
| `always_on_top`  | `true`            | Keep the window above other windows.               |
| `yellow_mode`    | `"any_inactive"`  | `any_inactive` or `active_wins`.                   |
| `poll_ms`        | `1500`            | Watcher backstop poll interval (clamped 250–60000).|
| `show_done`      | `false`           | Show every `done` session instead of the newest 5. |
| `notifications`  | `false`           | Desktop notification when a session needs help.    |
| `start_at_login` | `false`           | Launch at login. Off unless you turn it on.        |

Environment override: `AGENTLIGHT_STATE_PATH`. Window size and position are
persisted automatically by `tauri-plugin-window-state`.

## Pointing the container at a host folder

AgentLight only needs to read a file, so any channel works. For opencode inside
Docker, bind-mount clawlight's state directory out to the host (read-write, so
Remove works):

```powershell
docker run ... -v %USERPROFILE%\clawlight:/home/opencode/.claude/clawlight ...
```

Then set `state_path` to `%USERPROFILE%\clawlight\state.json` (or pick it in
Settings). A read-only mount is fine too; the light and details still work and
only Remove and Clear done are unavailable.

## Limitations

- **No process reaping from the host.** clawlight marks a session `done` when the
  recorded `owner_pid` is dead. That PID belongs to the container's PID
  namespace, so a host-side reader cannot check it. AgentLight applies only the
  same 24h staleness rule and otherwise trusts the file. A crashed session may
  stay `active` until clawlight itself writes `ended`/`done`.
- **Advisory lock across mounts.** See the caveat under Removing sessions.
- **`done` retention.** The details view keeps the newest 5 `done` sessions
  unless "Show every done session" is enabled.
- **One state file at a time.** See [`docs/ROADMAP.md`](docs/ROADMAP.md).

## Credits

AgentLight is an independent reader of the state file written by
[clawlight](https://github.com/clawlight/clawlight-cli) (MIT). It contains no
clawlight code; its status model and palette follow that project so the two
agree on what the light means.
