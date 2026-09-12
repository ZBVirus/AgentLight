# Pushing agent events to a hub

AgentLight's default source is a clawlight `state.json` file. A hub can also run
in **events mode**, where agents report normalized session state directly instead
of writing a shared file. This document covers the producer side: the
`agentlight-hook` CLI, the reference opencode plugin, and the `SessionEvent`
wire shape. The hub endpoint itself is specified in
[`protocol.md`](protocol.md#post-apiv1ingest).

The file source remains first-class. Events mode is opt-in; running a hub in
events mode does not read or write any file.

## 1. Run the hub in events mode

Start `agentlight-server` with `AGENTLIGHT_SOURCE=events` (or `ingest`). Set a
token so the ingest endpoint is authenticated — a `needs_help` push can open a
red light, so it is worth protecting.

```bash
export AGENTLIGHT_SOURCE=events
export AGENTLIGHT_TOKEN=secret
export AGENTLIGHT_DEVICES_FILE=/tmp/agentlight-devices.json
agentlight-server
```

- `AGENTLIGHT_BIND` defaults to `127.0.0.1:8787`; exposing it on the LAN is an
  explicit change.
- `AGENTLIGHT_STATE_FILE` is ignored in events mode.
- Sessions live in memory only: a restart starts empty, and `GET
  /api/v1/snapshot` reports `source_kind: "push"`, `source_label: "events"`,
  and — before the first event — `ok: false` with `Waiting for agent events`.

`POST /api/v1/ingest` requires the admin or a device token and accepts an
`{ "events": [ ... ] }` batch.

## 2. `agentlight-hook`

`agentlight-hook` is a small blocking CLI that reads events and POSTs them to
the hub. Build it from the workspace:

```bash
cargo build -p agentlight-hub-client --bin agentlight-hook
# or install it on PATH:
cargo install --path crates/agentlight-hub-client --bin agentlight-hook
```

Usage:

```bash
# single event object on stdin
echo '{"session_id":"abc","status":"active","name":"Fix auth"}' | agentlight-hook

# a batch via --json
agentlight-hook --json '{"events":[{"session_id":"abc","status":"needs_help"}]}'
```

It reads a single `SessionEvent` object **or** an `{ "events": [ ... ] }`
envelope, from stdin or `--json`. On success it prints `accepted N event(s)`
and exits `0`; on any parse, transport, or HTTP failure it prints a message to
stderr and exits non-zero. The token is never printed.

Environment:

| Variable | Default | Meaning |
|---|---|---|
| `AGENTLIGHT_HUB_URL` | `http://127.0.0.1:8787` | Hub base URL. |
| `AGENTLIGHT_TOKEN` | *(none)* | Bearer token sent when set. |

```bash
export AGENTLIGHT_HUB_URL=http://192.168.1.20:8787
export AGENTLIGHT_TOKEN=secret
echo '{"session_id":"abc","status":"active"}' | agentlight-hook
```

`AGENTLIGHT_TOKEN` is read from the environment by design: a CLI flag would
leak the secret into the process list and shell history.

## 3. Reference opencode plugin

[`plugins/opencode/agentlight.js`](../plugins/opencode/agentlight.js) is a real
plugin for the documented [opencode plugin API](https://opencode.ai/docs/plugins).
It subscribes to the event bus and forwards status with `fetch`, coalescing
bursts per session. It is a reference: it was written against the documented API
and event names but has **not** been exercised against a live opencode build in
this repository. Adapt the `handleEvent` switch if your version's event names or
payloads differ.

Install it by copying the file into a plugin directory opencode loads at
startup:

```bash
mkdir -p ~/.config/opencode/plugins
cp plugins/opencode/agentlight.js ~/.config/opencode/plugins/
# project-local instead: .opencode/plugins/agentlight.js
```

Configure it through the environment opencode is launched with:

```bash
export AGENTLIGHT_HUB_URL=http://127.0.0.1:8787
export AGENTLIGHT_TOKEN=secret   # only if the hub requires auth
```

Mapping (opencode → AgentLight):

| opencode event | `status` |
|---|---|
| `session.status` `busy` / `retry` | `active` (working) |
| `session.status` `idle`, `session.idle` | `inactive` (paused) |
| `permission.asked` / `permission.updated` | `needs_help` (waiting on you) |
| `permission.replied` | `active` (resumes) |
| `session.error` | `needs_help` |
| `session.deleted` | `done` |

`name` and `project_path` come from the session (`title`, `directory`);
`harness` is `opencode` and `last_updated` is stamped at send time. Transient
hub failures are logged and dropped; the next event reports the current state
again, and the last state seen by the hub wins because events are upserts keyed
by `session_id`.

Any integration can report the same way — shell out to `agentlight-hook` or POST
`/api/v1/ingest` directly. Only `session_id` and `status` are required.

## 4. `SessionEvent` schema

An ingest batch is `{ "events": [ SessionEvent, ... ] }`. Unknown fields on the
envelope and on each event are ignored, and re-pushing a `session_id` replaces
its prior status in place.

| Field | Type | Notes |
|---|---|---|
| `session_id` | string | Required. Upsert key. Keep it stable for a session. |
| `status` | string | Required. `active` \| `inactive` \| `needs_help` \| `done`. |
| `name` | string \| null | Display name; falls back to `Session <id prefix>`. |
| `project_path` | string \| null | Raw project path; empty when unknown. |
| `harness` | string \| null | Reporting harness; its two-char badge is derived. |
| `last_updated` | string \| null | RFC 3339 timestamp, echoed and used for ordering. |

The response is `{ "accepted": N }`, where `N` is the batch length, not the
number that changed.

## 5. Security

The hub token is a **credential**. Anyone who can reach the hub and present it
can inject sessions — including a `needs_help` that shows a red light — and, on
a token that also authorizes writes, remove sessions. Treat it like a password:

- Prefer a **device token** from pairing when a producer only needs ingest; it
  carries no admin rights.
- `agentlight-server` speaks plaintext HTTP. Treat the LAN as the trust
  boundary and use a VPN (WireGuard/Tailscale) for off-LAN access; TLS is not
  part of this protocol version.
- Bind to loopback unless you have opted into LAN exposure, and never print or
  commit the token. The hub stores only a SHA-256 hash of its own admin token,
  but the producer must present the plaintext, so keep it in the environment or
  a protected file.
