# Roadmap

Planned additions, roughly in priority order. Nothing here is committed to a
release; it is a parking lot so decisions are not silently forgotten.

For the proposed source/engine/transport rework behind these items, see
[`architecture-redesign.md`](architecture-redesign.md).

Status legend: **Done** shipped (merged into `main`); **Partial**
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
  and reference opencode plugins exist: `plugins/opencode/agentlight.js` for
  the V1 plugin API and `plugins/opencode/agentlight-v2.js` for the V2 API
  (`setup(ctx)` + `ctx.event.subscribe()` + `event.data`). The V1 plugin has
  been validated against a live opencode build; the V2 plugin is covered by the
  Node test suite.
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

**Status: Done** for the base feature (opt-in, off by default). The app
registers its `AppUserModelID` under `HKCU\Software\Classes\AppUserModelId`
on startup, so unpackaged/portable builds can toast too (an installed build
already gets an identity from the NSIS shortcut). **Planned** refinements:
per-harness titles, a "needs help" summary notification instead of one per
session, click-to-open the window from the toast, and a tray-balloon fallback
for machines where toasts are disabled by policy.

## Smaller things

- Auto-detect common state file locations on first run. **Planned.**
- A "waiting for clawlight" animation on the light while the file is absent.
  **Planned** (a text placeholder exists, no animation).
- Light/dark theme toggle (v1 is dark only). **Planned.**
- Tray menu entries for the state path and a manual refresh. **Planned.**
- Optional compact mode: show only the light, hide the top bar until hover.
  **Done:** the collapsed view is already light-only with no bar. A hover-reveal
  bar was not needed.
- `agentlight-source-events` is in the CI test and clippy matrix, and CI now
  also runs the jsdom frontend suite and the Playwright suite.
  **Done.**

## Future updates (recorded, not scheduled)

**Status: Planned.** Captured from product feedback. None of these are started;
decide and prioritize later.

- **Collapsed view customization. Done (v0.5 line).** Settings has the three
  light colors (`mini_red` / `mini_orange` / `mini_green`, any CSS color) with a
  "Reset colors" button, and a labels toggle (`mini_show_labels`, off by
  default). The colors apply to the lights and status chips in **both** the
  collapsed and the detail views; leaving the built-in color keeps the default
  palette. The gray/off color was removed (the triple light has no off state).
  Per-status custom rules are still open.
- **Collapsed view resizing. Reverted (v0.5 line).** A resizable frameless
  window exposes all eight OS resize edges and the framework has no
  corner-only mode; the aspect-snapping attempt fought the user's drags (an
  edge drag snapped back to the previous size). The collapsed view is
  fixed-size again at the per-style design size. A custom corner grip driving
  `set_size` (or an explicit diagonal drag) is the open option if resizing is
  wanted back; see `src-tauri/src/lib.rs` `resize_window`.
- **Topmost over full-screen apps. Planned (was tried, then removed).** An
  opt-in `topmost_reassert` re-asserted `SetWindowPos(HWND_TOPMOST)` every ~3 s,
  but it did not keep the widget above a Microsoft Store windowed-fullscreen
  window in testing, and it also overrode the pin. Removed for now: the pin /
  "Always on top" setting is the single authority on topmost. Revisit with a
  proper Windows-side experiment (foreground-change hook, or accepting that an
  active fullscreen window outranks a non-activated topmost window).
- **Plugin auto-starts the server. Done (v0.5 line).** Opt-in via
  `AGENTLIGHT_AUTOSTART_BIN`: the plugin probes `/healthz`, and if the hub is
  unreachable it spawns that binary detached, waits for health, and never owns
  the process lifecycle. No-op when unset.
- **`done` is never observed (bug). Fixed in v0.4.0.** Finished sessions,
  including subagents, only went idle, so "Clear done" missed them. The plugin
  now reports idle on a session with a `parentID` (a subagent) as `done`, and
  keeps an idle session that is waiting on a permission as `needs_help`.
  Covered by `tests/plugin/agentlight.test.js`.
- **Right-click hide in collapsed mode. Done (v0.5 line).** The collapsed view
  has a context menu with a "Hide" action that sends the app to the tray.
- **Stay on the same monitor when expanding/collapsing. Done (v0.5 line).** The
  window now remembers the collapsed rectangle's home monitor — the one with the
  most overlap, ties broken by the top-left corner, then the primary — and
  expands/collapses within that monitor, preserving the left edge. A window
  exactly on the seam is therefore stable. Verified by unit tests on the
  pure geometry helpers in `src-tauri`.
- **Click-outside collapses Settings without losing the draft. Planned.** Today
  an outside click tucks the expanded detail view back to the light when
  "Always on top" is on, but Settings is deliberately left alone so an
  in-progress edit is not lost. Planned: an outside click while Settings is
  open should collapse the window too, and clicking the light again should
  reopen Settings exactly as it was — same scroll offset, same unsaved edits —
  without saving, as if the page had never been closed. Discard stays explicit
  (Back or Esc). Needs the frontend to preserve the Settings draft across the
  view switch and clicking the light to reopen Settings rather than Details.
- **More harnesses for the "Open" link. Planned.** The session link is built for
  opencode only; other leading harnesses (Claude Code, Codex CLI, GitHub Copilot
  CLI, Antigravity, …) get no link and no Open button until their web-UI URL
  shapes are known.
- **Subagent visibility. Done (v0.5 line).** Producers tag tool-spawned child
  sessions `subagent: true`. The desktop hides them by default and only fires "A
  session finishes" for main sessions, with an opt-in "Show subagent sessions"
  and a separate "A subagent finishes" trigger. A subagent that needs help still
  turns the light red so a prompt is never missed. The standalone hub relays
  subagent rows with their `is_subagent` flag so every client applies its own
  toggle; the hub no longer filters them, which used to make the desktop's
  toggle a no-op for hub sources. The built-in web client hides them by default.
- **Jump to a session in the opencode web UI. Done (v0.5 line, best-effort).**
  The detail view shows an "Open" action built from the app's **Session link**
  setting: OpenCode v2 by default (`<base>/server/<base64url(base)>/session/<id>`,
  matching `opencode pair`), with `opencode_v1`, `producer` (the hub/plugin URL
  from `AGENTLIGHT_SESSION_URL_TEMPLATE`), and `off` options. The server address
  is remembered per mode (`session_link_bases`; the legacy single
  `session_link_base` migrates on load), so switching modes never carries a
  stale address. Exact browser window/tab focus is still not possible from a
  native app; the browser decides whether to reuse or open a tab. Full focus
  would need a browser extension or remote debugging.
- **Attention alarms and custom sounds. Done (v0.5 line).** `alarms_enabled`
  plays a sound on an alarm edge; `alarm_trigger` (and `notification_trigger`)
  accept **any combination** of `needs_help`, `done`, and `any_status`, and the
  edge fires on the first match; `alarm_sound` chooses a `.wav` file (default is
  a bundled two-note chime played through the Win32 sound API). Edges are
  computed in the engine, separate from the notification toast. The alarm rows
  hide unless alarms are enabled. Per-source rules, snooze, and quiet hours
  remain open.
- **Producer startup reconcile. Planned.** Known symptom: a session last
  reported `active` can stay `active` indefinitely once the producer goes
  quiet. The hub is a durable store of the last reported state, and nothing
  corrects it: the plugin never posts an empty snapshot (deliberate, so a
  starting instance cannot wipe the hub), a session that was active when
  opencode/plugin stopped is never re-seen, and an event that arrived while the
  hub was down is dropped. The 24h staleness backstop (push sessions render as
  `done` when `last_updated` is older than a day, in-memory only) bounds the
  "forever" case but does not fix a recent one.
  - **Safe fix (preferred): producer reconcile at plugin startup.** List
    opencode's sessions, seed ones the plugin has not seen as `paused`, then
    the heartbeat/snapshot can prune truthfully — including an empty set when
    opencode really has no sessions. Needs the session-list API shape for the
    supported opencode versions; guessing statuses without a recency bound
    would surface every historical session as paused.
  - **Alternative: surface producer silence.** Running the hub with
    `AGENTLIGHT_HEARTBEAT_MS` set makes a silent producer report as a source
    error ("producer has not reported since …") after 4× that window, hiding
    stale rows. Config-only, but it also hides legitimate last-known state.
  - A shorter staleness window (e.g. 6h) is a constant/config decision if the
    24h backstop proves too slow in practice.
- **Removed sessions stay removed in events mode. Fixed (v0.5 line).**
  `EventPushSource` persists tombstone ids: a producer heartbeat (`snapshot`)
  cannot resurrect a session removed with the X button or "Clear done", while a
  real upsert event clears the tombstone and brings it back. Covered by
  `crates/agentlight-source-events` tests.
- **Desktop receives pushed plugin events directly (embedded events mode).
  Planned.** Today the desktop's embedded server is always built in file mode
  (`AppState::with_devices` without `.with_events`), so `POST /api/v1/ingest`
  answers `400 this server is not in events mode`, and the desktop source
  builder maps `SourceKind::Push` back to the local file. Feeding the widget
  from the plugin therefore needs a standalone `agentlight-server` in events
  mode plus `Source = hub`. Planned: let the embedded server host the same
  `EventPushSource` as the standalone server, so the plugin can point straight
  at the desktop and the ingested sessions render with no separate hub process.
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

## Round-6 backlog (recorded 2026-10-14)

Captured from the round-6 testing feedback. Statuses updated as the follow-up
work landed.

- **Settings dirty-state feedback. Planned.** Back/Esc discard silently today.
  Mark the form when it differs from the saved config (dot on the Save button
  or a short "unsaved changes" hint) and/or dim/disable Save when nothing
  changed, so a discard is never a surprise.
- **Hub health in Settings. Done (round-6 follow-up).** The hub fields show a
  `/healthz` probe: reachability plus the hub binary's version and protocol
  schema (`GET /healthz` gained an additive `version` field). The result is
  cached and probed at startup, when the hub source changes, and by the
  Refresh button beside the field — opening or saving Settings never waits on
  it. A file source skips the probe.
- **Diagnostics log. Done (round-6 follow-up).** The shell appends to
  `%APPDATA%\AgentLight\agentlight.log` (rotated to `agentlight.log.1` when
  oversized at startup): startup, source switches, embedded-server
  transitions, notifications/alarms, config saves, and errors, plus a panic
  hook. Settings has a "Copy diagnostics" action that puts a redacted report
  (versions, source/hub state, log tail) on the clipboard; tokens never leave
  the config.
- **`show_done` through a hub. Planned.** A hub renders only its newest five
  `done` rows (its own `show_done`, default off), so a client's "Show every
  done session" cannot bring older ones back. Decide between a hub-side
  setting/env (`AGENTLIGHT_SHOW_DONE`) and relaying all rows with client-side
  capping.
- **Bulk Clear done over a hub. Done (round-6 follow-up).** A source can now
  advertise `bulk_clear_done` (the hub and the push store do; the file source
  does not, because it must also remove stale-reaped rows). The engine then
  issues one `clear_done` command instead of a removal per session, so a hub
  clear is one POST plus one refresh instead of N of each.
- **Optimistic Remove (pending-removal overlay). Planned.** Today Remove waits
  for the hub to acknowledge before the row disappears. The alternative is to
  hide the row immediately and keep a local pending-removal set, so the next
  snapshot — which still contains the session until the hub processes it —
  cannot flash it back; reconcile on the hub's update, and roll back with an
  error if the hub rejects or is unreachable. Not worth it on a LAN, where the
  wait is one short POST round trip. It becomes worthwhile once the hub is the
  hosted service over the internet (see "North star"), with tens-to-hundreds of
  milliseconds of RTT and flaky links. The overlay must survive concurrent
  snapshots, coexist with the hub's tombstones and other clients, and always
  have a rollback path, so it is a small design, not a one-line change.
- **Copy session details. Done (round-6 follow-up).** The global
  `user-select: none` now has overrides for commit-worthy text — session names
  and sub-lines, labels, hints, the pairing code, and form fields — so names
  and paths can be selected and copied. A dedicated copy action is still open.
- **Tray tooltip with status. Planned.** `TrayIcon::set_tooltip` updated on
  each engine update ("2 working · 1 needs help") makes the hidden widget
  glanceable without showing the window.
