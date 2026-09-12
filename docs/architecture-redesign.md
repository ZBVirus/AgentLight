# AgentLight architecture redesign

Status: design accepted; Phases 0-3 and most of Phase 5 are implemented on
`feat/architecture-redesign` (statuses per phase in section 8). Scope: how to make AgentLight modular enough to grow into a
multi-client product (desktop, web, mobile) without breaking clawlight
compatibility or the current desktop widget.

This document records the as-built architecture, the forces acting on it, a
target architecture, the concrete differences and their effects, and a staged
migration plan. It intentionally does not change behavior; it is the design
input for the work that follows.

---

## 1. Why now

The widget works and its boundaries are already good: all state semantics live
in `agentlight-core`, which has no GUI or platform dependencies, and the Tauri
shell is thin. What the current shape cannot do is grow outward:

- Only one state source exists (a clawlight `state.json` on a filesystem the
  app can read).
- The wire format is a single `Snapshot` built for one frontend, with no
  schema version.
- Watching, retry, notification edge detection, and window behavior all live
  in `src-tauri`, so any second shell (mobile, CLI, server) would have to copy
  them or depend on Tauri.
- Configuration mixes "what to read" with "how the desktop window behaves".
- There is no network surface, so a phone cannot connect, and notifications
  are desktop-local only.

Adding mobile and remote notifications is a transport problem, not a parsing
problem. The redesign is therefore about introducing a source/adapter seam, an
engine that owns behavior, and a versioned transport, while keeping the
clawlight file reader exactly as it is.

---

## 2. Current architecture (as built)

```
                     Windows host
+--------------------------------------------------------------+
|  src-tauri (Tauri v2 shell)                                  |
|                                                              |
|  commands: get_snapshot get_config set_config clear_session   |
|            clear_done pick_state_file set_always_on_top       |
|            resize_window window_minimize window_hide quit_app |
|                                                              |
|  tray (show/hide, settings, quit)                            |
|  watcher: notify + poll backstop (spawn_watcher)             |
|  notifications: edge-triggered in maybe_notify               |
|  startup: autostart, single instance, window state           |
|                       |                                      |
|          builds snapshots on demand / on file change         |
+-----------------------|--------------------------------------+
                        v
+--------------------------------------------------------------+
|  crates/agentlight-core (no GUI, no async, no Tauri)         |
|                                                              |
|  state.rs     parse state.json, statuses, aggregate,         |
|               reap_stale (24h), .state.lock, atomic write,   |
|               clear_session / clear_done on raw Value        |
|  session.rs   display rows: names, badges, ordering,         |
|               done retention (newest 5)                      |
|  config.rs    desktop prefs + state path resolution          |
|  snapshot.rs  build_snapshot_at(path, config, now) -> Snapshot|
+--------------------------------------------------------------+
                        ^
                        | IPC (invoke) + events ("state-changed")
+--------------------------------------------------------------+
|  dist/ (static HTML/CSS/JS, no bundler)                      |
|  app.js: view state, render, drag, settings, mini/full sizes |
+--------------------------------------------------------------+
```

### 2.1 Component responsibilities

| Component | Owns | Does not own |
|---|---|---|
| `agentlight-core` | clawlight file parsing, status model, aggregate, staleness, session display rows, snapshot assembly, atomic clear | watching, timing, IPC, OS integration |
| `src-tauri` | process lifecycle, tray, window, watcher scheduling, notifications, autostart, config persistence | parsing rules, aggregate semantics |
| `dist` | presentation and local view state | file IO, business rules |

### 2.2 Data flow today

1. `spawn_watcher` watches the directory of `state.json` with `notify`, plus a
   poll backstop every `poll_ms` (default 1500 ms) for bind mounts that emit no
   events.
2. On change it calls `emit_snapshot`, which calls
   `agentlight_core::build_snapshot`.
3. `build_snapshot_at` reads and permissively parses the file, applies the 24h
   staleness reap, computes counts and the aggregate, builds display rows, and
   returns a JSON `Snapshot`.
4. The shell emits `state-changed`; the frontend re-renders.
5. Remove/Clear are explicit writes: best-effort `.state.lock`, read raw JSON,
   delete keys, temp file + rename, preserving unknown fields.

### 2.3 Invariants that must survive

These are product and compatibility guarantees, not implementation details.

1. **Clawlight compatibility.** `docs/state-format.md` is the contract. Reading
   is permissive; an unknown or malformed session is skipped, not fatal.
2. **Unknown-field preservation.** Every write works on the raw
   `serde_json::Value`; fields AgentLight does not model survive untouched.
3. **Reads never lock.** clawlight writes atomically, so a reader always sees a
   complete document.
4. **Never write after a failed parse.** A broken file is never overwritten.
5. **No host PID checks.** `terminal.owner_pid` is namespaced to the container;
   only the 24h `last_updated` rule is available host-side.
6. **Exact aggregate semantics.** Any `needs_help` is red; otherwise
   `any_inactive` (default) any idle is orange, else any active is green, else
   gray. `active_wins` is the alternate mode.
7. **Truncate by characters, not bytes.** Names are arbitrary UTF-8.
8. **Core stays GUI-free.** `agentlight-core` must keep building and testing on
   Linux with no Tauri, glib, or Windows dependencies.

### 2.4 Where the current shape strains

- **One source, hard-wired.** `Config` has a single `state_path`; the watcher
  owns exactly one path. `docs/ROADMAP.md` already wants several.
- **Behavior in the shell.** Notification edge detection, watcher lifecycle,
  and poll policy cannot be reused by a server or mobile shell.
- **Snapshot is both domain result and wire format.** No `schema_version`, so
  clients cannot evolve independently.
- **Desktop assumptions in config.** `poll_ms`, `always_on_top`,
  `start_at_login`, and `state_path` sit in one flat struct used by `config.rs`,
  `snapshot.rs`, and the shell.
- **No identity for sessions across sources.** Session id is only the key in
  the clawlight map; a second source would collide.
- **No network surface.** Nothing to connect to; no auth, no pairing, no
  history.
- **Frontend is one file.** `app.js` mixes store, rendering, drag, and settings.

---

## 3. Requirements and forces

### Functional

- Keep every current desktop behavior and the clawlight file contract.
- Support more than one source eventually (several state files, several hosts,
  a push-based source).
- Allow a remote client (phone, another desktop) to view state and receive
  "needs help" notifications.
- Allow future agents to report state without writing a shared file, if that
  becomes desirable.

### Non-functional

- Simple to run: no account, no cloud, no database required for the default
  setup.
- Privacy first: state stays on the user's machines unless they opt into
  remote access.
- Reuse the Rust domain logic everywhere; do not reimplement status semantics
  per platform.
- Testability: parsing and policy tests keep running on any host, without a
  GUI, network, or clock.
- Modest dependency surface. The core crate should stay small and synchronous.

### Explicitly out of scope for the first cut

- Accounts, billing, and a hosted control plane.
- Storing long-term history or analytics.
- Executing actions inside containers (focus, attach, kill).
- Replacing clawlight. We remain a reader and a compatibility layer.

---

## 4. Design principles

1. **Adapters at the edge, rules in the middle.** Sources translate the
   outside world into one normalized model; the engine applies policy; shells
   only present and translate user intent back through commands.
2. **The file is a source, not the architecture.** clawlight's `state.json`
   becomes one implementation of a `StateSource` trait, not the shape of the
   whole system.
3. **Split when a second consumer exists.** Do not preemptively carve crates
   nobody imports; split modules now, extract crates when the server or mobile
   client actually lands.
4. **Version the wire, not the world.** One `schema_version` integer; changes
   are additive within a major version; clients ignore unknown fields.
5. **Sync core, async edges.** Keep `agentlight-core` synchronous and
   runtime-free. Async runtimes and sockets belong to server/client crates.
6. **Capabilities over assumptions.** A source declares what it can do
   (remove, clear, push) so the UI can disable what is impossible.
7. **Preserve the guarantees in section 2.3.** They are acceptance criteria,
   not aspirations.

---

## 5. Proposed architecture

### 5.1 Overview

```
  Sources (adapters)            Engine (policy)             Surfaces / Clients
+---------------------+     +----------------------+     +---------------------+
| ClawlightFileSource |     |  SourceRegistry      |     | Desktop shell        |
|  notify + poll      | --> |  RetentionPolicy     | --> |  (Tauri, embedded)   |
|  lock + atomic write|     |  Aggregate           |     |                      |
+---------------------+     |  NotificationPolicy  |     | HTTP/WS/SSE server   |
| ClawlightHttpSource |     |  ConfigStore         | --> |  (optional, opt-in)  |
|  (remote hub client)|     |  Revisioned updates  |     |                      |
+---------------------+     +----------------------+     | Web/PWA client       |
| EventPushSource     |                                  | Mobile client        |
|  (future hooks/CLI) |                                  | CLI / TUI            |
+---------------------+                                  +---------------------+
```

The engine is the only component that knows how to turn source snapshots into
the product's model. Every surface consumes the same `Snapshot` and the same
update stream. A source can be local (a file) or remote (a hub); a server can
be embedded in the desktop process or run standalone.

### 5.2 Normalized domain model

The clawlight types stay exactly as they are inside the adapter. The engine
speaks a normalized model with stable identity:

```rust
pub struct SourceId(String);            // "local", "laptop-docker", ...

pub struct SessionKey {
    pub source: SourceId,
    pub session_id: String,             // clawlight map key
}

pub struct Session {
    pub key: SessionKey,
    pub name: String,                   // already truncated by chars
    pub status: Status,                 // reuses current Status
    pub project: Option<String>,
    pub harness: Option<String>,
    pub badge: Option<String>,
    pub updated_at: Option<DateTime<Utc>>,
    pub is_done: bool,                  // after retention/staleness
}

pub struct SourceSnapshot {
    pub source: SourceId,
    pub revision: u64,
    pub observed_at: DateTime<Utc>,
    pub health: SourceHealth,           // Ready | Missing | Unreadable(reason)
    pub sessions: Vec<Session>,
    pub capabilities: Capabilities,
}

pub struct Snapshot {
    pub schema_version: u32,            // starts at 1
    pub generated_at: DateTime<Utc>,
    pub aggregate: String,              // red | orange | green | gray
    pub counts: Counts,
    pub sources: Vec<SourceSnapshot>,
    pub sessions: Vec<Session>,         // merged, sorted
}
```

`Status`, `Aggregate`, `Counts`, and the display rules move unchanged; only
identity and framing are new. Sessions are keyed by `(source, session_id)` so
two sources cannot collide.

### 5.3 Source adapters

```rust
pub trait StateSource: Send + Sync {
    fn id(&self) -> SourceId;
    fn capabilities(&self) -> Capabilities;
    /// Full current view. Cheap and idempotent.
    fn snapshot(&self, now: DateTime<Utc>) -> SourceSnapshot;
    /// Register a callback for revision changes. The engine decides how to
    /// debounce and merge.
    fn subscribe(&self, sink: Arc<dyn Fn(SourceEvent) + Send + Sync>);
    /// Optional commands, gated by `capabilities`.
    fn command(&self, command: SourceCommand) -> Result<()>;
}

pub struct Capabilities {
    pub remove_session: bool,
    pub clear_done: bool,
    pub push_events: bool,
}

pub enum SourceCommand {
    RemoveSession(SessionKey),
    ClearDone,
}
```

Implementations:

| Adapter | Backing | Capabilities | Notes |
|---|---|---|---|
| `ClawlightFileSource` | `state.json` + `notify`/poll | remove, clear | today's behavior, extracted verbatim |
| `ClawlightHubSource` | remote agentlight server | per remote | desktop/mobile client mode |
| `EventPushSource` | HTTP/CLI events (future) | depends | for agents that can emit directly |
| `FixtureSource` | in-memory | none | deterministic tests and demos |

The `ClawlightFileSource` is a direct lift of `state.rs` plus the watcher that
currently lives in `spawn_watcher`. Nothing about parsing, locking, atomic
writes, unknown-field preservation, or the 24h reap changes.

### 5.4 Engine

```rust
pub struct Engine {
    sources: Vec<Arc<dyn StateSource>>,
    config: ConfigStore,
    subscribers: Vec<Arc<dyn Fn(Update) + Send + Sync>>,
}

impl Engine {
    pub fn snapshot(&self, now: DateTime<Utc>) -> Snapshot;
    pub fn subscribe(&self, sink: Arc<dyn Fn(Update) + Send + Sync>);
    pub fn dispatch(&self, command: SessionCommand) -> Result<()>;
}

pub struct Update {
    pub revision: u64,      // monotonic across the engine
    pub snapshot: Snapshot, // snapshots are small; ship them whole
    pub notifications: Vec<Notification>,
}
```

Responsibilities that move out of `src-tauri` and into the engine:

- Source lifecycle: start watching, restart on config change (today's
  `generation` counter).
- Merge policy across sources (per-source and global aggregate).
- Retention (`show_done`, newest 5) and staleness, already in core.
- Notification edge detection (today's `maybe_notify`): emit
  `Notification { key, title, body, urgency }` when a session transitions into
  `needs_help`. Shells decide how to deliver them.
- Debounce and revision assignment.

The engine stays synchronous. A server wraps it: `spawn_blocking` to compute,
then `tokio::sync::broadcast` for fan-out. This keeps `agentlight-core` free of
tokio and Tauri and preserves the "tests on any host" property.

### 5.5 Surfaces

**Desktop shell (`src-tauri`).** Keeps process lifecycle, tray, window,
autostart, and desktop toasts. It constructs an engine with a
`ClawlightFileSource` and forwards `Update`s to the frontend over the existing
`state-changed` event. Commands become thin calls into `Engine::dispatch`.

**Server (`agentlight-server`, new crate, opt-in).** Exposes the engine over
HTTP for other devices:

```
GET  /api/v1/snapshot              -> Snapshot (JSON, schema_version)
GET  /api/v1/events                -> SSE stream of Update frames
WS   /api/v1/stream                -> same stream over WebSocket (fallback)
POST /api/v1/commands              -> { "command": "remove_session", ... }
GET  /healthz                      -> liveness, no auth
```

- Binds to `127.0.0.1` by default; LAN exposure is an explicit setting.
- Snapshots are small (single-digit kilobytes), so shipping whole snapshots
  keeps the protocol simple. Incremental deltas are a later optimization.
- Auth: a pairing code shown on the desktop, exchanged for a per-device bearer
  token; tokens are revocable. No passwords, no accounts.
- TLS: optional self-signed for LAN; remote access should go through a VPN
  (WireGuard/Tailscale) rather than a bespoke relay, at least initially.

**Web/PWA client.** The existing `dist` app can run against a hub URL with a
token. It is also the fastest path to mobile: installable PWA with Web Push
(VAPID) for "needs help" notifications, no app-store process.

**Mobile app (later).** Tauri v2 mobile can reuse `agentlight-core` and a
`ClawlightHubSource` client crate, giving native notifications and background
connectivity. Alternatively, a small native shell over the same HTTP API. The
protocol, not the platform, decides; the domain stays in Rust.

### 5.6 Configuration

Split "what to read" from "how this shell behaves":

```jsonc
{
  "schema_version": 1,
  "sources": [
    { "id": "local", "kind": "clawlight_file",
      "path": "C:/Users/me/clawlight/state.json", "poll_ms": 1500 }
  ],
  "ui": {
    "collapse_style": "single",
    "always_on_top": true,
    "show_done": false,
    "yellow_mode": "any_inactive"
  },
  "notifications": { "desktop": false, "push": false },
  "start_at_login": false,
  "server": { "enabled": false, "bind": "127.0.0.1:0", "lan": false }
}
```

Backward compatibility: the existing flat `config.json` is read by a migration
that creates one `clawlight_file` source from `state_path` + `poll_ms` and maps
the rest into `ui`. Old fields remain accepted on read; writes use the new
shape.

### 5.7 Notifications pipeline

Today: edge detection in the Tauri shell, delivered as a desktop toast.
Proposed:

1. Engine emits `Notification` values alongside `Update`s when a session
   enters `needs_help` (deduplicated per `SessionKey`, cleared when resolved).
2. A delivery layer chooses channels: desktop toast, SSE event for connected
   clients, Web Push/APNs/FCM for mobile, or an external tool such as `ntfy`
   or UnifiedPush.
3. Each channel is independent and optional, configured under `notifications`.

This makes "notify my phone" a transport plugin instead of a rewrite.

---

## 6. File vs server: analysis and recommendation

### 6.1 Options

1. **File-only (today).** Agents write `state.json`; AgentLight reads it.
2. **Local hub.** One process (embedded in the desktop app or standalone) owns
   the file sources and serves snapshots/events to other devices on the LAN.
3. **Event ingest.** Agents (via a small hook/CLI) POST normalized events to a
   hub; files remain supported as a source.
4. **Cloud relay.** A hosted service fans out between hosts and mobile across
   networks.

### 6.2 Comparison

| Dimension | File-only | Local hub | Event ingest | Cloud relay |
|---|---|---|---|---|
| Latency | poll, 250 ms–60 s | push, sub-100 ms | push, sub-100 ms | push + WAN RTT |
| Devices | one host | LAN clients | LAN clients | anywhere |
| Offline | fully | hub local, clients need LAN | hub local | needs internet |
| Setup cost | none | low (opt-in server) | medium (hook install) | high (accounts, infra) |
| Auth surface | OS file perms | pairing token | token | accounts + E2E |
| History | none | in memory | in memory, extensible | database |
| Agent changes | none | none | hook per agent | hook per agent |
| Failure modes | stale file | port, firewall, hub down | missed events | relay outage, privacy |
| Privacy | best | good (LAN only) | good | needs E2E encryption |
| Clawlight compat | native | native (via file) | file still works | file still works |

### 6.3 Recommendation

Do not replace the file; make it one source behind a seam. Concretely:

- **Keep `ClawlightFileSource` as the default and the compatibility guarantee.**
  The file contract is clawlight's, not ours, and is the only thing agents
  write today.
- **Add the hub as an optional transport.** The desktop app embeds it (off by
  default, loopback only). A standalone `agentlight-server` exists for
  headless hosts. Mobile and web clients connect over the LAN with a pairing
  token.
- **Add event ingest later, and only as a complement.** A small
  `agentlight hook` CLI or HTTP endpoint lets future agents push without a
  shared file, but the file source stays first-class.
- **Skip the cloud relay for now.** Use Tailscale/WireGuard for off-LAN access
  until there is a concrete need; then the relay can reuse the same auth and
  schema, with end-to-end encryption over the normalized event model.

This keeps the default install exactly as simple as it is today: one process,
one file, no network. Every additional capability is opt-in.

Chosen for the first hub: topology A, a hub embedded in the desktop app.
Topology B, a standalone `agentlight-server` running as a container sidecar, is
recorded in [`ROADMAP.md`](ROADMAP.md) as a deferred deployment. It reuses the
same engine, binary, and wire protocol, so adding it later is a deployment
choice rather than a redesign.

---

## 7. Key differences and their effects

| Area | Today | Proposed | Effect |
|---|---|---|---|
| Source coupling | one `state_path` in `Config` | `StateSource` trait + registry | several files/hosts; remote sources; test fixtures |
| Behavior location | watcher + notifications in Tauri | engine owns policy | mobile/CLI reuse semantics; thin shells |
| Identity | session id only | `(source_id, session_id)` | multi-source without collisions; stable for clients |
| Wire format | `Snapshot`, unversioned | `Snapshot` + `schema_version` | independent client evolution; additive changes |
| Transport | IPC + events | IPC + HTTP/SSE/WS | phone and web can connect; same payload |
| Notifications | desktop toast only | engine emits, channels deliver | phone push, external tools, per-channel policy |
| Config | flat desktop struct | nested sources + ui + server | independent growth; migration preserved |
| Core dependencies | sync, small | unchanged | tests still run anywhere; async stays at edges |
| Writes | direct file lock/write | through source capabilities | remote clients can request, host executes |
| Offline | works | works (hub is local, optional) | no regression in the default path |

Effects worth calling out:

- **Latency.** Remote clients see updates when the hub emits, not on the file
  poll. The file poll remains the floor for file-based sources.
- **Security.** Exposing a hub adds a listening socket and a token lifecycle.
  Default loopback plus explicit LAN opt-in keeps the blast radius small.
- **Failure isolation.** A hub that is down, or a phone off the network,
  degrades only the transport; the desktop keeps working from the file.
- **Scope discipline.** Each new source is a small adapter; the domain and
  engine do not grow with the number of integrations.
- **Protocol is the product boundary.** HTTP + JSON is easy to test and
  debug, and `schema_version` makes breaking it a conscious act.

---

## 8. Migration plan

Each phase is shippable and leaves the app working.

**Phase 0 — Baseline (done).** Widget with clawlight file reads, tray,
notifications, settings, release pipeline.

**Phase 1 — Extract the seam (no behavior change). Status: Done.**
- Add `source` module to `agentlight-core` with `StateSource`,
  `SourceSnapshot`, `Capabilities`, `SessionKey`.
- Move `ClawlightFileSource` (parsing + watcher loop + poll backstop) into the
  module. Keep public functions as thin wrappers for compatibility.
- Move notification edge detection and merge/retention policy into an
  `engine` module.
- Rewrite `src-tauri` commands as engine calls; delete duplicated watcher and
  notification code.
- Exit criteria: all 34 tests still pass; new adapter/engine tests; zero UI
  change.

**Phase 2 — Frontend modules. Status: Done.**
- Split `dist/app.js` into ES modules: store, render, views, drag, ipc. Keep
  no bundler and `withGlobalTauri`.
- Exit criteria: same behavior; each module testable in isolation.

**Phase 3 — Local hub. Status: Done** (protocol, endpoints, pairing and
per-device tokens, desktop embedding).
- New `agentlight-server` crate: `axum` (or equivalent) serving
  `/api/v1/snapshot` and `/api/v1/events`, loopback by default.
- Embedded in the desktop app behind a setting; standalone binary for
  headless hosts.
- Pairing-code auth and a per-device token store.
- Exit criteria: desktop unchanged when disabled; a second machine on the LAN
  renders the same snapshot.

**Phase 4 — Web/PWA + mobile client. Status: Partial.** The Rust client crate
and a same-origin browser client exist; the PWA, push, and mobile shell do not.
- `ClawlightHubSource` client crate (Rust) and a PWA mode of `dist`.
- Web Push or `ntfy`/UnifiedPush for notifications.
- Tauri mobile shell if native background/push is needed.
- Exit criteria: phone shows the light and receives "needs help" alerts.

**Phase 5 — Optional event ingest and multi-source UX. Status: Partial.**
Ingest, `EventPushSource`, the `agentlight-hook` CLI, and a reference opencode
plugin exist; the multi-source UI does not.
- `agentlight hook` CLI / HTTP endpoint; merge policy and per-source labels in
  the UI; `docs/protocol.md` published.
- Exit criteria: an agent can report without a shared file, and the file
  source still works unchanged.

---

## 9. Target crate layout

```
crates/
  agentlight-core/            domain + engine + source trait (sync, no GUI)
    src/state.rs              clawlight parsing, aggregate, lock, atomic write
    src/session.rs            display rows, retention
    src/snapshot.rs           snapshot assembly + schema_version
    src/source.rs             StateSource trait, normalized model
    src/source/clawlight.rs   file adapter (notify + poll)
    src/engine.rs             registry, merge, notifications, updates
    src/config.rs             nested config + migration from the flat form
  agentlight-server/          HTTP/SSE/WS surface, auth, embedding API
  agentlight-client/          Rust client for the hub protocol
src-tauri/                    desktop shell: window, tray, autostart, toasts
dist/                         static frontend
apps/mobile/                  future Tauri mobile shell
docs/
  state-format.md             clawlight contract (unchanged)
  architecture-redesign.md    this document
  protocol.md                 hub protocol (created in Phase 3)
```

Split rule: extract a crate when a second consumer exists. Phase 1 keeps
everything in `agentlight-core`; `agentlight-server` and `agentlight-client`
appear only when Phase 3/4 need them.

---

## 10. Testing strategy

- **Contract tests for the clawlight adapter.** Keep fixtures per clawlight
  version and assert parsing, aggregate, staleness, and unknown-field
  preservation through clear.
- **Property tests for writes.** Removing a session must not change any other
  key or value in the raw document.
- **Deterministic engine tests.** Injected clock and a `FixtureSource`; assert
  revision monotonicity, merge order, retention, and notification edges.
- **Protocol tests.** Golden JSON for `schema_version` 1; a compatibility test
  that a client built for v1 ignores a v2-added field.
- **Server integration tests.** In-process hub with a fake source; assert
  snapshot, stream ordering, auth rejection, and revocation.
- **No GUI in tests.** The core must keep building and testing on Linux with
  no Tauri, glib, or Windows dependencies.

---

## 11. Security and privacy

- Default bind is loopback; LAN requires an explicit toggle.
- Pairing code with a short lifetime; per-device tokens; revoke per device.
- Commands (remove, clear) are host-executed and gated by
  `Capabilities`; a read-only client cannot mutate.
- No telemetry by default; no state leaves the machine unless the user enables
  a transport.
- Remote access over the internet should first be solved with a VPN. A future
  relay must be end-to-end encrypted and self-hostable.
- Document what each surface exposes and what it does not.

---

## 12. Risks and open questions

- **Scope creep.** The hub is the point where a widget becomes a platform.
  Keep phases small and keep the default path free of network code.
- **Async leakage.** Resist making `agentlight-core` async; it hurts
  testability and drags tokio into every consumer.
- **Protocol drift.** Two model layers (clawlight types and normalized types)
  must not diverge; the adapter owns the translation and only the adapter
  changes when clawlight changes.
- **Mobile background limits.** iOS/Android restrict long-lived connections;
  push notifications or periodic sync are required for timely alerts.
- **Multi-source UX.** Per-source aggregates, labels, and "which host is red"
  need design; defer until a second source is real.
- **Write authority.** With multiple clients, last-writer-wins on the file is
  fragile. Commands should be serialized by the host that owns the source.
- **Naming.** Whether to rename `agentlight-core` to `agentlight-engine` when
  the server lands.

---

## 13. Non-goals

- Replacing clawlight or writing to agents.
- A hosted service that is required for the default experience. A hosted hub
  with accounts is the eventual north star (see `ROADMAP.md`), but it stays
  optional and self-hosting stays first-class.
- Long-term history, metrics, or team dashboards.
- Container process control (focus, attach) from the host.
- A plugin system beyond source adapters and notification channels.

---

## 14. Summary

The current split is already close to the right shape: a GUI-free core and a
thin shell. The missing piece is a seam. Introducing `StateSource` and an
engine moves behavior out of Tauri, gives sessions stable identity across
sources, and turns the file into one adapter among several. A versioned
snapshot over HTTP/SSE then makes every future client — web, PWA, mobile,
CLI — a consumer of the same contract, while the desktop default stays exactly
as simple as it is today and clawlight compatibility is untouched.
