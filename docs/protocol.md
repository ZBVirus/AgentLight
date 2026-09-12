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

- Optional static bearer token. When the server is configured with a token, every
  `/api/v1/*` request must send `Authorization: Bearer <token>`, or supply it as
  a `?token=<token>` query parameter. The query form exists for browser
  `EventSource`, which cannot set request headers. Token comparison is
  constant-time for both.
- `GET /healthz` and `GET /` are always open, so the built-in client can load
  before the user supplies a token.
- Missing or invalid token: `401` with the error shape below. Token comparison
  is constant-time.

## Error shape

Every error response is JSON:

```json
{ "error": { "code": "unauthorized", "message": "missing or invalid bearer token" } }
```

| `code` | HTTP | Meaning |
|---|---|---|
| `unauthorized` | 401 | Missing or wrong bearer token. |
| `bad_request` | 400 | Malformed body, unknown command, or no source to target. |
| `command_failed` | 409 | The source could not apply the command (e.g. unreadable state). |
| `internal_error` | 500 | Unexpected server failure. |

## `GET /`

The built-in, self-contained web client: a single HTML document with inline CSS
and JavaScript that reads the token from `location.search`, fetches
`/api/v1/snapshot`, and subscribes to `/api/v1/events`. No auth, no build step,
no third-party assets. It is same-origin with the API, so no CORS is required.

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

## `GET /api/v1/snapshot`

Returns `agentlight_core::Snapshot` JSON:

| Field | Type | Notes |
|---|---|---|
| `ok` | bool | State file was read and parsed. |
| `error` | string \| null | Waiting/read error when `ok` is false. |
| `state_path` | string | Resolved clawlight `state.json` path. |
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

## Server defaults

`agentlight-server` binds `127.0.0.1:8787` unless overridden
(`AGENTLIGHT_BIND`), and auth is disabled unless `AGENTLIGHT_TOKEN` is set.
`AGENTLIGHT_STATE_PATH` points at clawlight's `state.json` and
`AGENTLIGHT_POLL_MS` sets the watcher backstop. LAN exposure is an explicit
opt-in; use a VPN for off-LAN access.

The same server can be embedded in the desktop app with
`agentlight_server::start(engine, config) -> ServerHandle`, which runs its own
Tokio runtime on a background thread and returns the bound address. The
pairing-code flow (Phase 3 step 3.2) remains deferred; until it lands, clients
use the static bearer token.
