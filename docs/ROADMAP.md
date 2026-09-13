# Roadmap

Planned additions, roughly in priority order. Nothing here is committed to a
release; it is a parking lot so decisions are not silently forgotten.

For the proposed source/engine/transport rework behind these items, see
[`architecture-redesign.md`](architecture-redesign.md).

Status legend: **Done** shipped on `feat/architecture-redesign`; **Partial**
some pieces exist; **Planned** recorded, not started; **Deferred** intentionally
later. Statuses are as of the v0.4.0 line.

## North star (end goal)

**Status: Planned.** The design is shaped for it; none of the service exists.

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

- **A — desktop-embedded hub (chosen). Status: Done.** The desktop app owns the
  engine and serves clients over the LAN. Lowest cost: no new process. Downside:
  the hub only exists while the desktop app runs; PC off means no clients.
- **B — container sidecar hub (deferred). Status: Partial.** A standalone
  `agentlight-server` runs inside the container next to opencode and clawlight,
  owns the engine, and serves desktop and phone clients. Independent of the
  desktop PC, so the phone works with the PC off. The binary exists and CI ships
  a Windows build; container packaging, supervision, and discovery are not done.
  Downside: a new process to build, run, supervise, expose, secure, and update
  in the container, plus container networking and discovery. Build B later; the
  protocol is the same as A.
- **C — hosted service (north star). Status: Planned.** A hosted hub with
  accounts where the plugin pushes state and clients subscribe. See "North star"
  above. Deferred, but the protocol and auth model are shaped to allow it.

## opencode plugin as a source

Replace clawlight's file write with an opencode plugin that reports session
state directly. This skips `state.json` entirely. Two shapes:

- **Push. Status: Done, validated live.** The plugin POSTs events to the hub's
  ingest endpoint. `EventPushSource`, `POST /api/v1/ingest`, `agentlight-hook`,
  and a reference opencode plugin exist. The plugin has since been validated
  against a live opencode build.
- **Pull. Status: Planned.** The plugin exposes a small HTTP/SSE source endpoint
  the hub subscribes to.

The plugin is a source, never the engine. The clawlight file source stays as a
first-class alternative for backward compatibility.

## Multiple containers / state files

**Status: Partial.** The source seam, `(source, session_id)` keys, and
per-source label and health exist. There is no multi-source config, no
per-source aggregate, and no per-source color yet.

Today AgentLight reads exactly one `state.json`. Clawlight itself supports one
state file per machine, and the common setup is one container, so this is fine
for v1.

Later:

- Watch several `state.json` files at once (for example two opencode containers
  with separate volumes) and merge their sessions into one view. **Planned.**
- Give each source a label and a color, and show the source name on each row.
  **Partial:** labels and health are in the snapshot; colors and per-row source
  names are not.
- Make `state_path` accept a list in `config.json`, keeping the single-string
  form as shorthand for a one-element list. **Planned.**
- Per-source aggregate vs. a global aggregate, selectable in settings.
  **Planned.**

The core parsing layer already takes an explicit state path, so this is mostly a
matter of iterating sources in the Tauri shell and keying sessions by
`(source, session_id)`.

## Click-to-focus a session

**Status: Deferred.** clawlight's TUI raises the terminal window that hosts a
session. A host app cannot do that for a session running inside a container,
because the terminal lives in the container's process namespace. Revisit only if
AgentLight ever reads a state file written by a host-local clawlight.

## Desktop notifications

**Status: Done** for the base feature (opt-in, off by default). **Planned**
refinements: per-harness titles, a "needs help" summary notification instead of
one per session, and click-to-open the window from the toast.

## Smaller things

- Auto-detect common state file locations on first run. **Planned.**
- A "waiting for clawlight" animation on the light while the file is absent.
  **Planned** (a text placeholder exists, no animation).
- Light/dark theme toggle (v1 is dark only). **Planned.**
- Tray menu entries for the state path and a manual refresh. **Planned.**
- Optional compact mode: show only the light, hide the top bar until hover.
  **Done:** the collapsed view is already light-only with no bar. A hover-reveal
  bar was not needed.

## Future updates (recorded, not scheduled)

**Status: Planned.** Captured from product feedback. None of these are started;
decide and prioritize later.

- **Collapsed view customization. Done (v0.5 line).** Settings now has the four
  collapsed-light colors (`mini_red` / `mini_orange` / `mini_green` /
  `mini_gray`, any CSS color) and a labels toggle (`mini_show_labels`). The
  colors apply to all three styles; leaving the built-in color keeps the
  default palette. Per-status custom rules are still open.
- **Collapsed view resizing. Done (v0.5 line).** Only the collapsed window is
  user-resizable; its size persists to `mini_width` / `mini_height` and is
  restored on restart. Expanded views keep their fixed sizes. Saves are
  coalesced so a drag writes at most once per ~500 ms.
- **Topmost over full-screen apps. Done (v0.5 line).** Opt-in
  `topmost_reassert` re-asserts always-on-top every ~3 s (while `always_on_top`
  is on) so a borderless/exclusive full-screen app cannot push the widget
  behind. Windows is the target; a dedicated native `SetWindowPos` re-assert
  may still be needed if the Tauri call proves insufficient.
- **Plugin auto-starts the server. Done (v0.5 line).** Opt-in via
  `AGENTLIGHT_AUTOSTART_BIN`: the plugin probes `/healthz`, and if the hub is
  unreachable it spawns that binary detached, waits for health, and never owns
  the process lifecycle. No-op when unset.
- **`done` is never observed (bug). Fixed in v0.4.0.** Finished sessions,
  including subagents, only went idle, so "Clear done" missed them. The plugin
  now reports idle on a session with a `parentID` (a subagent) as `done`, and
  keeps an idle session that is waiting on a permission as `needs_help`.
  Covered by `plugins/opencode/test/agentlight.test.js`.
- **Right-click hide in collapsed mode. Done (v0.5 line).** The collapsed view
  has a context menu with a "Hide" action that sends the app to the tray.
- **Stay on the same monitor when expanding/collapsing. Done (v0.5 line).** The
  window now remembers the collapsed rectangle's home monitor — the one with the
  most overlap, ties broken by the top-left corner, then the primary — and
  expands/collapses within that monitor, preserving the left edge. A window
  exactly on the seam is therefore stable. Verified by unit tests on the
  pure geometry helpers in `src-tauri`.
- **Jump to a session in the opencode web UI. Done (v0.5 line, best-effort).**
  The plugin sets `url` from `AGENTLIGHT_SESSION_URL_TEMPLATE` (for example
  `http://localhost:4096/session/{id}`), the wire carries it on the session row,
  and the detail view shows an "Open" action. Exact browser window/tab focus is
  still not possible from a native app; the browser decides whether to reuse or
  open a tab. Full focus would need a browser extension or remote debugging.
- **Attention alarms and custom sounds. Done (v0.5 line).** `alarms_enabled`
  plays a sound on an alarm edge; `alarm_trigger` selects `needs_help`, `done`,
  or `any_status`; `alarm_sound` chooses a `.wav` file (default is the system
  beep). Edges are computed in the engine, separate from the notification toast.
  Per-source rules, snooze, and quiet hours remain open.
- **Deferred engineering options. Planned.** From the architecture review:
  - Tag and release the architecture line: hub, pairing, file or hub source,
    ingest, and the plugin.
  - **Done (v0.4.x).** Producer resync and heartbeat, plus a durable push
    store, so a hub restart recovers state instead of waiting for the next
    event. The heartbeat carries only the live set (non-`done` sessions plus a
    bounded recent-`done` tail, never full history), and a `mode: "snapshot"`
    ingest prunes sessions that vanished while the hub was down. Documented in
    `docs/protocol.md` under "Snapshot mode and producer heartbeat".
  - **Done (v0.4.x).** `HubSource` follows the hub's SSE stream, emitting a
    change on connect and on each `update`/`lagged` frame and reconnecting on
    disconnect. If the stream cannot be established it falls back to a one-shot
    `GET /api/v1/snapshot` poll, emitting only on change.
