# Roadmap

Planned additions, roughly in priority order. Nothing here is committed; it is a
parking lot so decisions are not silently forgotten.

For the proposed source/engine/transport rework behind these items, see
[`architecture-redesign.md`](architecture-redesign.md).

## North star (end goal)

The long-term product is a hosted hub service ("the service") with accounts.

- The opencode plugin pushes session state to the service. The plugin is the
  producer; clawlight and a shared file are no longer required.
- The service fans the state out to every client linked to the account:
  desktop, web, PWA, mobile, CLI.
- One account authenticates both sides. The plugin is registered to the account
  as a producer; each client device is registered as a consumer. Linking a
  device to the account is what authorizes it to see state and receive
  notifications.
- This removes the need for a VPN or a reachable host. It is the convenient
  path for people who do not want to run Tailscale/WireGuard or expose ports.
- Self-hosting stays first-class. A user who wants full control wires their own
  transport (LAN, Tailscale, WireGuard, port-forward) and runs the engine
  themselves: no account, no dependency on the service.
- The service is cheap by design. Small JSON status payloads, no history by
  default, no agent content, only session status. Cost scales with the number
  of connections, not data volume.

Design constraints to keep open now:

- The wire protocol must be identical whether the hub is local or hosted, so
  the service is a transport swap, not a rewrite. See `docs/protocol.md`.
- Authentication must separate producer (plugin) from consumer (clients), with
  account linking and per-device revocation.
- Privacy: prefer end-to-end encryption between producer and clients so the
  service holds only ciphertext and routing metadata. At minimum, no session
  names or project paths in service logs.
- The local topologies (A desktop hub, B container sidecar) remain supported.
  The service is topology C.

Recorded so the design stays compatible. Not a near-term deliverable.

## Hub topologies

The hub is the engine plus an HTTP/SSE surface for remote clients (desktop,
phone, web, CLI). Several deployment topologies are planned. They run the same
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
- **C — hosted service (north star).** A hosted hub with accounts where the
  plugin pushes state and clients subscribe. See "North star" above. Deferred,
  but the protocol and auth model are shaped to allow it.

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

## Future updates (recorded, not scheduled)

Captured from product feedback. None of these are committed or started; decide
and prioritize later.

- **Collapsed view customization.** Let the user choose what the collapsed
  window shows and which conditions map to which lights: per-condition colors,
  which statuses light up, and possibly custom rules. Today the styles are
  single, triple, and triple-vertical, with fixed colors.
- **Collapsed view resizing.** Let the user resize the collapsed window to any
  size they want, rather than the fixed size per style. Consider how the
  expanded window behaves after a resize.
- **Topmost over full-screen apps.** On Windows, another window can hide
  AgentLight even with always-on-top, for example an exclusive or borderless
  full-screen game such as Rocket League. Investigate the full-screen/topmost
  interaction and whether a native topmost re-assert or a different window
  style is needed.
- **Plugin auto-starts the server.** Let the opencode plugin start the hub when
  it is not running. Must work both for opencode on the host and for opencode
  inside a container. Needs discovery of a running hub, spawn and permission
  rules, and a decision on who owns the process lifecycle.
- **`done` is never observed (bug).** Finished sessions, including subagent
  sessions that have clearly stopped, are not labeled `done`, so "Clear done"
  misses them. Decide whether the plugin should emit `done` on session end or
  the client should infer it, and make finished sessions clearable.
- **Right-click hide in collapsed mode.** Add a context menu on the collapsed
  window with a "Hide" action that sends the app to the tray.
- **Deferred engineering options.** From the architecture review:
  - Tag and release the architecture line: hub, pairing, file or hub source,
    ingest, and the plugin.
  - Producer resync and heartbeat, plus an optional durable push source, so a
    hub restart recovers state instead of waiting for the next event.
  - Test the opencode plugin against a live opencode build and adjust the
    event names if they differ.
  - Move `HubSource` from polling to the hub's SSE stream.
