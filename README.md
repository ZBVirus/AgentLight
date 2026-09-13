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

- **Collapsed** — the mini window: a single aggregate light, or three per-status
  lights, horizontal or vertical traffic-light style. Click to expand, drag to
  move. Chosen in Settings.
- **Details** — aggregate light plus live counts, then every session: status
  dot, name, harness badge (`oc` / `cx` / `co`), project, relative time, and a
  remove button. A "Clear done" action drops all `done` rows.
- **Settings** — source (local file or remote hub), state path (with a file
  picker) or hub URL and token, collapsed view, always-on-top, start at login,
  notifications, show-every-done, idle behavior, and the poll interval.

The window is frameless and draggable by its top bar, and it lives in the system
tray: closing or hiding it keeps it running, and the tray icon toggles it back.
Left-clicking the tray icon shows/hides the window; the tray menu has Settings
and Quit.

## Serving to other devices

Settings can start an embedded HTTP server over the same engine. It is off by
default and binds loopback; exposing it on the LAN is an explicit choice.

- With neither an admin token nor a paired device, the API is open (the loopback
  default).
- With an admin token set, every `/api/v1/*` request needs a valid token.
- With a paired device but no admin token, `/api/v1/*` needs a device token, and
  HTTP device management (`GET /api/v1/devices`,
  `DELETE /api/v1/devices/{id}`) returns `403`. The desktop can still list and
  revoke devices from Settings because it calls the server in-process.

The admin token is stored only as a SHA-256 hash, and device tokens are hashed
the same way; plaintext tokens are never persisted, so a saved admin token
cannot be shown again. Pair other devices with the pairing code. The standalone
binary reads `AGENTLIGHT_TOKEN`, `AGENTLIGHT_BIND`, and
`AGENTLIGHT_DEVICES_FILE` (default `devices.json`). The full rule is in
[`docs/protocol.md`](docs/protocol.md).

For a headless or container host, build the standalone `agentlight-server`
binary with `cargo build -p agentlight-server`; CI builds it on Windows and
tagged releases attach it.

## Reading from a hub

The desktop can read its sessions from either a local `state.json` (the default)
or a remote AgentLight hub, chosen under **Source** in Settings. The embedded
server above is independent: this widget can host a hub and read from one at the
same time, and reading from a hub needs no local clawlight install.

A hub that requires auth is addressed with a bearer token. That token is a
**client credential** the hub validates, so it must be sent verbatim and cannot
be stored as a hash the way the server's admin token is. It therefore lives in
**plaintext** in `config.json`; protect that file as you would any secret. It is
never logged. Point `hub_url` at the host running `agentlight-server` (or at
another desktop with its embedded server enabled).

Status text follows the chosen source: a hub that cannot be reached says so
("Could not reach the hub"), instead of talking about a state file.

## Push from an agent (no file)

A hub can also run in **events mode** (`AGENTLIGHT_SOURCE=events`), where agents
report session state directly instead of writing a shared `state.json`. The
producer side ships two pieces:

- `agentlight-hook` — a CLI that reads a `SessionEvent` (or a batch) from stdin
  or `--json` and POSTs it to `POST /api/v1/ingest`.
- A reference **opencode plugin**, validated against a live opencode build, that
  forwards `session` / `tool` / `permission` events to the hub.

```bash
export AGENTLIGHT_SOURCE=events AGENTLIGHT_TOKEN=secret
agentlight-server &
echo '{"session_id":"abc","status":"active","name":"Fix auth"}' | agentlight-hook
```

See [`docs/plugin.md`](docs/plugin.md) for hub setup, the hook, plugin install,
the `SessionEvent` schema, and the token security note.

## How it reads state

- Resolution order for the state file: `AGENTLIGHT_STATE_FILE` env var, then
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
crates/agentlight-core/        Rust library, no GUI deps, unit-tested everywhere
  src/state.rs                 parse state.json, aggregate, reap, lock, clear, atomic write
  src/session.rs               display rows: names, badges, ordering, done retention
  src/source.rs                StateSource trait + normalized session model
  src/source/clawlight.rs      file adapter: notify + poll, lock + atomic write
  src/engine.rs                merge, retention, aggregate, notifications, source lifecycle
  src/config.rs                preferences + path resolution
  src/snapshot.rs              the JSON payload the frontend renders
crates/agentlight-server/       hub: routes, auth, devices, embedded web client
crates/agentlight-hub-client/   HubClient + HubSource + the agentlight-hook CLI
crates/agentlight-source-events/ EventPushSource for agents that push events
src-tauri/                     Tauri v2 shell (Windows/macOS/Linux)
  src/lib.rs                   commands, tray, watcher, autostart, notifications
  tauri.conf.json              frameless transparent always-on-top window
dist/                          static HTML/CSS/JS frontend (no bundler, no Node)
docs/                          state contract, roadmap
scripts/make_icons.py          regenerates the app icons
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
cargo tauri build              # NSIS installer under target/release/bundle/nsis/
cargo tauri build --no-bundle  # portable target/release/agentlight.exe
```

The portable exe needs the WebView2 runtime, which ships with Windows 10/11.
For quick testing without an install, download the `agentlight-windows-portable`
artifact from the latest CI run, or the `*_portable.exe` asset on a release.

CI (`.github/workflows/ci.yml`) runs the core tests on Linux and builds the
portable exe on Windows; the release workflow attaches both the portable exe and
the NSIS installer to a tagged release.

## Configuration

Config file: `%APPDATA%\AgentLight\config.json`. It is created on first save and
can be edited by hand.

| Field            | Default           | Meaning                                            |
|------------------|-------------------|----------------------------------------------------|
| `state_path`     | *(resolved)*      | Absolute path to `state.json`.                     |
| `source_kind`    | `"file"`          | `file` reads `state_path`; `hub` reads `hub_url`; `push` is a server-only mode. |
| `hub_url`        | `http://127.0.0.1:8787` | Remote hub base URL when `source_kind` is `hub`. |
| `hub_token`      | *(none)*          | Bearer token sent to the hub. Stored plaintext.    |
| `always_on_top`  | `true`            | Keep the window above other windows.               |
| `yellow_mode`    | `"any_inactive"`  | `any_inactive` or `active_wins`.                   |
| `collapse_style` | `"single"`        | `single`, `triple`, or `triple_vertical`.          |
| `poll_ms`        | `1500`            | Watcher backstop poll interval (clamped 250–60000).|
| `show_done`      | `false`           | Show every `done` session instead of the newest 5. |
| `notifications`  | `false`           | Desktop notification when a session needs help.    |
| `start_at_login` | `false`           | Launch at login. Off unless you turn it on.        |
| `server_enabled` | `false`           | Start the embedded hub. Off unless you turn it on. |
| `server_bind`    | `127.0.0.1:8787`  | Address the embedded hub binds.                    |

Environment override: `AGENTLIGHT_STATE_FILE`. Window size and position are
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
