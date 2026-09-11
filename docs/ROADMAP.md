# Roadmap

Planned additions, roughly in priority order. Nothing here is committed; it is a
parking lot so decisions are not silently forgotten.

For the proposed source/engine/transport rework behind these items, see
[`architecture-redesign.md`](architecture-redesign.md).

## Hub topologies

The hub is the engine plus an HTTP/SSE surface for remote clients (desktop,
phone, web, CLI). Two deployment topologies are planned. They run the same
engine, binary, and protocol, so switching later is a deployment choice, not a
rewrite.

- **A — desktop-embedded hub (chosen).** The desktop app owns the engine and
  serves clients over the LAN. Lowest cost: no new process. Downside: the hub
  only exists while the desktop app runs; PC off means no clients.
- **B — container sidecar hub (deferred).** A standalone `agentlight-server`
  runs inside the container next to opencode and clawlight, owns the engine,
  and serves desktop and phone clients. Independent of the desktop PC, so the
  phone works with the PC off. Downside: a new process to build, run,
  supervise, expose, secure, and update in the container, plus container
  networking and discovery. Build B later; the protocol is the same as A.

## opencode plugin as a source

Replace clawlight's file write with an opencode plugin that reports session
state directly. This skips `state.json` entirely. Two shapes:

- **Push.** The plugin POSTs events to the hub's ingest endpoint.
- **Pull.** The plugin exposes a small HTTP/SSE source endpoint the hub
  subscribes to.

The plugin is a source, never the engine. The clawlight file source stays as a
first-class alternative for backward compatibility.

## Multiple containers / state files

Today AgentLight reads exactly one `state.json`. Clawlight itself supports one
state file per machine, and the common setup is one container, so this is fine
for v1.

Later:

- Watch several `state.json` files at once (for example two opencode containers
  with separate volumes) and merge their sessions into one view.
- Give each source a label and a color, and show the source name on each row.
- Make `state_path` accept a list in `config.json`, keeping the single-string
  form as shorthand for a one-element list.
- Per-source aggregate vs. a global aggregate, selectable in settings.

The core parsing layer already takes an explicit state path, so this is mostly a
matter of iterating sources in the Tauri shell and keying sessions by
`(source, session_id)`.

## Click-to-focus a session

clawlight's TUI raises the terminal window that hosts a session. A host app
cannot do that for a session running inside a container, because the terminal
lives in the container's process namespace. Revisit only if AgentLight ever
reads a state file written by a host-local clawlight.

## Desktop notifications

Implemented (opt-in, off by default). Possible refinements: per-harness titles,
a "needs help" summary notification instead of one per session, and click-to-open
the window from the toast.

## Smaller things

- Auto-detect common state file locations on first run.
- A "waiting for clawlight" animation on the light while the file is absent.
- Light/dark theme toggle (v1 is dark only).
- Tray menu entries for the state path and a manual refresh.
- Optional compact mode: show only the light, hide the top bar until hover.
