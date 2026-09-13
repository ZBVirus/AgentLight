# AgentLight hub protocol

Wire contract for `agentlight-server`. Current `schema_version`: **1**.

The payloads are the Rust types in `agentlight-core`, unchanged: `Snapshot`
for reads and `Update` for the event stream. This document describes the HTTP
framing around them, not a second model.

## Versioning

- `schema_version` is `1`. It is reported by `GET /healthz` and encoded in the
  `/api/v1` path prefix.
- Changes within a version are **additive**: new optional fields, new SSE event
  names, new capability strings. Clients **must ignore unknown fields and
  unknown event names** and keep working.
- Removing or renaming a field, changing a value's meaning, or changing a
  status/aggregate string is breaking and bumps `schema_version` and the path
  prefix.
- `agentlight_core::Snapshot` does not carry `schema_version` in its body yet;
  the path prefix plus `/healthz` are authoritative for now.

## Authentication

Two kinds of bearer token are accepted:

- **Admin token.** The optional static operator secret. `AGENTLIGHT_TOKEN` on
  the standalone binary, or the desktop's "Admin token" setting. It authorizes
  every endpoint, including device management.
- **Device token.** A per-device secret obtained by pairing (below). It
  authorizes the read/command endpoints but not device management. A device
  token stays valid until it is revoked.

A request may carry the token as `Authorization: Bearer <token>`, or as a
`?token=<token>` query parameter. The query form exists for browser
`EventSource`, which cannot set request headers.

### What is required

- **No admin token and no paired device.** The API is open. This is the loopback
  default: nothing is listening beyond `127.0.0.1` unless the operator opted
  into LAN exposure.
- **Admin token hash set.** Every `/api/v1/*` request needs a valid token. The
  admin token works everywhere; a device token works on the read/command
  endpoints only.
- **A paired device but no admin token.** `/api/v1/*` needs a device token, and
  the HTTP device-management endpoints (`GET /api/v1/devices`,
  `DELETE /api/v1/devices/{id}`) return `403 forbidden`. There is no admin
  secret to present, so HTTP clients cannot manage devices.
- The desktop app can still list and revoke paired devices from Settings even
  with no admin token, because it calls the running server in-process rather
  than over HTTP.

`GET /healthz` and `GET /` are always open, so the built-in client can load
before the user supplies a token. Missing or invalid credentials: `401` with the
error shape below.

### Storage

The admin token is stored only as a SHA-256 hex digest (`server_token_hash` on
the desktop; the standalone binary hashes `AGENTLIGHT_TOKEN` at startup). Device
tokens are hashed the same way. Comparison is constant-time over the digests.
Plaintext tokens are never persisted, and a saved admin token cannot be shown or
recovered later — pair additional devices with the pairing code instead. A
legacy config file that still carries the plaintext `server_token` field is
migrated to `server_token_hash` on load, and the plaintext is not written back.

`agentlight-server` and the embedded hub do plaintext HTTP. Treat the LAN as the
trust boundary and use a VPN (WireGuard/Tailscale) for off-LAN access; TLS is
not part of this version.

## Error shape

Every error response is JSON:

```json
{ "error": { "code": "unauthorized", "message": "missing or invalid bearer token" } }
```

| `code` | HTTP | Meaning |
|---|---|---|
| `unauthorized` | 401 | Missing or wrong token, or a bad/expired pairing code. |
| `forbidden` | 403 | An endpoint that needs the admin token has none configured. |
| `not_found` | 404 | Unknown device id. |
| `bad_request` | 400 | Malformed body, unknown command, or no source to target. |
| `command_failed` | 409 | The source could not apply the command (e.g. unreadable state). |
| `internal_error` | 500 | Unexpected server failure. |

## `GET /`

The built-in, self-contained web client: a single HTML document with inline CSS
and JavaScript that reads the token from `location.search` or `localStorage`,
fetches `/api/v1/snapshot`, and subscribes to `/api/v1/events`. With no token it
shows a self-service pairing form that calls `POST /api/v1/pair` and remembers
the returned device token. No auth to load, no build step, no third-party
assets. It is same-origin with the API, so no CORS is required.

## `GET /healthz`

Liveness plus the negotiation data a client needs before authenticating. No auth.

```json
{
  "status": "ok",
  "schema_version": 1,
  "capabilities": {
    "events": ["sse"],
    "commands": ["remove_session", "clear_done"],
    "auth": "bearer"
  }
}
```

`capabilities.auth` is `"bearer"` or `"none"`. Clients negotiate by treating
unknown capability strings as unsupported; unknown fields are ignored.

## `POST /api/v1/pair`

No auth. Exchanges the pairing code shown on the host (or logged by the
standalone binary) for a per-device token. The code is eight characters from an
unambiguous alphabet, is valid for five minutes, and can pair more than one
device within that window.

Request:

```json
{ "code": "X7Y5R7K4", "device_name": "Pixel 8" }
```

`device_name` is optional; a blank name is stored as `Unnamed device`. Response:

```json
{ "device_id": "6c356f3262d6f27dc45161ae97630f32", "token": "…64 hex chars…" }
```

The plaintext `token` is returned exactly once. The server stores only its
SHA-256 hash, so a leaked device store cannot be replayed. A wrong or expired
code is `401 unauthorized`. The host rotates or regenerates the code from the
desktop; regenerating invalidates the previous code.

## `GET /api/v1/devices`

Admin token only. Lists paired devices. When no admin token is configured this
returns `403 forbidden`; a device token is not sufficient. The response never
contains a token or its hash:

```json
[
  {
    "id": "6c356f3262d6f27dc45161ae97630f32",
    "name": "Pixel 8",
    "created_at": "2026-09-12T21:25:49Z",
    "last_seen": "2026-09-12T21:26:10Z"
  }
]
```

## `DELETE /api/v1/devices/{id}`

Admin token only. Revokes a device; its token stops working immediately.
Returns `204 No Content`, or `404 not_found` for an unknown id.

## `GET /api/v1/snapshot`

Returns `agentlight_core::Snapshot` JSON:

| Field | Type | Notes |
|---|---|---|
| `ok` | bool | The active source was read and parsed. |
| `error` | string \| null | Waiting/read error when `ok` is false. Wording follows the source: a `file` source says "state file", a `hub` source says "hub". |
| `state_path` | string | Resolved clawlight `state.json` path. |
| `source_kind` | string | `file` \| `hub` \| `push`. Additive; older payloads omit it and are treated as `file`. |
| `source_label` | string | The active source location: a file path for `file`, a base URL for `hub`, the source id (`events`) for `push`. Additive. |
| `exists` | bool | The file exists (even if unreadable). |
| `aggregate` | string | `red` \| `orange` \| `green` \| `gray`. |
| `counts` | object | `needs_help`, `active`, `inactive`, `done`, `total`. |
| `sessions` | array | Display rows, see below. |
| `yellow_mode` | string | `any_inactive` \| `active_wins`. |
| `generated_at` | string | RFC 3339 timestamp. |

Each session row:

| Field | Type | Notes |
|---|---|---|
| `session_id` | string | Clawlight map key. |
| `name` | string | Display name. |
| `status` | string | `active` \| `inactive` \| `needs_help` \| `done`. |
| `status_label` | string | `working` \| `paused` \| `needs help` \| `done`. |
| `project_name` | string | Last path segment, or `unknown`. |
| `project_path` | string | Raw path, empty when unknown. |
| `harness` | string \| null | Reporting harness, if any. |
| `harness_badge` | string \| null | Two-char badge, already derived. |
| `last_updated` | string | Raw clawlight timestamp, echoed verbatim. |
| `is_done` | bool | Post-retention done marker. |

`done` sessions are capped at the newest five unless the host config sets
`show_done`.

When `ok` is false the `error` string is source-aware: a file-backed engine
reports `Waiting for clawlight state file` / `Could not read state file: …`,
a hub-backed engine reports `Waiting for the hub` / `Could not reach the hub: …`,
and a push-backed engine reports `Waiting for agent events` /
`Agent event source error: …`. Clients may show their own wording from
`source_kind` instead of echoing `error`.

## `GET /api/v1/events`

Server-Sent Events (`Content-Type: text/event-stream`). The server sends a
keep-alive comment every 15 seconds.

| Event | Data |
|---|---|
| `update` | A `agentlight_core::Update` object as JSON (below). |
| `lagged` | Decimal count of updates dropped for this client; re-fetch `/api/v1/snapshot`. |

An `Update` frame:

```json
{
  "revision": 7,
  "snapshot": { "...": "same shape as GET /api/v1/snapshot" },
  "notifications": [
    {
      "key": { "source": "local", "session_id": "abc" },
      "title": "AgentLight",
      "body": "\"Fix auth\" needs help",
      "urgency": "critical"
    }
  ]
}
```

`notifications` mirrors the engine's `agentlight_core::Notification`: `urgency`
is `low` \| `normal` \| `critical`, and an entry fires once when a session
transitions into `needs_help`. The standalone binary enables notifications for
its engine, so remote clients receive these edges; an embedder that leaves
`Config::notifications` off will always see an empty array.

**Revision semantics.** `revision` is a monotonic `u64`, incremented on every
engine publish (a source change or a command's follow-up refresh). Whole
snapshots are shipped each frame; there are no deltas. Revisions are global to
the engine, not per-source. A client should render only the newest revision it
has seen and ignore older ones. Under `lagged`, some revisions were skipped for
that client; because each frame is a full snapshot, re-fetching is optional but
recommended.

## `POST /api/v1/commands`

Auth required. The body is a tagged command.

Remove a session (the source id is part of the session key; `source` is
optional and defaults to the engine's first source):

```json
{ "command": "remove_session", "source": "local", "session_id": "abc" }
```

Clear every session the engine currently reports as `done` — including
sessions downgraded to `done` by the 24-hour staleness rule, not just those
whose file `status` is `done`. Unknown fields are preserved:

```json
{ "command": "clear_done" }
```

Success returns the revision produced by the refresh that follows the command:

```json
{ "revision": 8 }
```

The resulting state is also published on `/api/v1/events` for subscribers.
Commands are executed by the host that owns the source. Capability gating
(`Capabilities::remove_session` / `clear_done`) is not enforced by the server
yet; a client should consult future negotiation data before offering the
actions. Writes preserve unknown fields and never overwrite a state file that
failed to parse.

## `POST /api/v1/ingest`

Auth required (admin or device token). Available only when the server runs in
events mode (`AGENTLIGHT_SOURCE=events`); in file mode it returns
`400 bad_request`. It upserts a batch of pushed session events into the
in-memory event source, keyed by `session_id`, and returns how many were
accepted:

```json
{ "events": [
  {
    "session_id": "abc",
    "status": "needs_help",
    "name": "Fix auth",
    "project_path": "/work/agentlight",
    "harness": "opencode",
    "last_updated": "2026-09-12T21:25:49Z"
  }
] }
```

Each event:

| Field | Type | Notes |
|---|---|---|
| `session_id` | string | Required. Upsert key. |
| `status` | string | Required. `active` \| `inactive` \| `needs_help` \| `done`. |
| `name` | string \| null | Display name; falls back to `Session <id prefix>`. |
| `project_path` | string \| null | Raw project path, empty when unknown. |
| `harness` | string \| null | Reporting harness; its two-char badge is derived. |
| `last_updated` | string \| null | RFC 3339 timestamp, echoed and used for ordering. |

Unknown fields on the envelope and on each event are ignored. Re-pushing a
`session_id` replaces its prior status in place. The batch is a single change:
subscribers to `/api/v1/events` see one `update` afterward. Response:

```json
{ "accepted": 1 }
```

`accepted` is the number of events in the batch, not the number that changed.
Sessions are held in memory only today; there is no file and no history, so a
restart starts empty. `GET /api/v1/snapshot` reports `source_kind: "push"` and
`source_label: "events"`, and before the first event `ok` is `false` with the
error `Waiting for agent events`.

### Snapshot mode and producer heartbeat (planned, schema 1 additive)

Upsert alone cannot answer "what is the current state" after the hub or the
producer restarts. Two additive pieces fix that. Both are optional; omitting
them keeps today's behavior.

**`mode` on the ingest envelope.** Optional string, default `"upsert"`.

- `"upsert"` — merge the batch into the source, keyed by `session_id`. Today's
  behavior: only adds and updates.
- `"snapshot"` — the batch is the producer's authoritative **live set**: upsert
  the batch, then drop every push-source session whose `session_id` is absent
  from it. This prunes sessions that vanished while the hub was down. The batch
  is still a single change and still returns `{"accepted": N}`.

When implemented, `/healthz` advertises `capabilities.ingest` as
`["upsert", "snapshot"]`; a producer should treat an unknown or absent mode list
as upsert-only.

**What a heartbeat carries.** A producer that wants the hub to stay truthful
sends its live set on load and on a periodic heartbeat (recommended every 30s).
The live set is:

- every session that is not `done`, plus
- a bounded tail of recent `done` sessions, matching the engine's own retention
  (newest five unless `show_done`).

History is deliberately excluded: a heartbeat is **not** "every session that
ever existed." A full historical reconcile, if ever wanted, is a one-time
`snapshot` on producer startup, never a recurring beat. Fixing the `done` signal
for sessions that end without a delete is the correct fix; resending history is
not.

**Durability.** The push source persists its live set to
`AGENTLIGHT_EVENTS_FILE` and reloads it at startup, so the hub can report
last-known state before the first heartbeat. Restored rows keep their
per-session `last_updated`, so clients show the row's age instead of claiming
freshness. A hub with no ingest within a multiple of the heartbeat interval may
mark the source stale.

## Server defaults

`agentlight-server` binds `127.0.0.1:8787` unless overridden
(`AGENTLIGHT_BIND`), and admin auth is disabled unless `AGENTLIGHT_TOKEN` is set
(its SHA-256 hash is what the server keeps). `AGENTLIGHT_SOURCE` selects the
session source: `file` (default) reads clawlight's `state.json`, while `events`
(or `ingest`) serves sessions pushed to `POST /api/v1/ingest` and ignores
`AGENTLIGHT_STATE_FILE`. `AGENTLIGHT_STATE_FILE` points at clawlight's
`state.json`, `AGENTLIGHT_POLL_MS` sets the watcher backstop, and
`AGENTLIGHT_DEVICES_FILE` (default `devices.json` in the working directory)
is where paired devices persist. In events mode, `AGENTLIGHT_EVENTS_FILE`
(default `push-state.json`) is the **planned** durable store for the push
source; see "Snapshot mode and producer heartbeat" above. The pairing code is
logged at startup so a headless host can be paired. LAN exposure is an explicit opt-in; use a VPN for
off-LAN access.

The same server can be embedded in the desktop app with
`agentlight_server::start(engine, config) -> ServerHandle`, which runs its own
Tokio runtime on a background thread and returns the bound address. The desktop
shows the pairing code and device list in settings, backed by
`ServerHandle::pair_info`, `regenerate_pairing`, `devices`, and `revoke_device`.
