# clawlight `state.json` contract

Verified against clawlight `v0.13.0`
(`github.com/clawlight/clawlight-cli`, MIT). Line references are to that repo's
`main` branch at the time of writing. If upstream changes, re-check
`src/state.rs` and `src/session.rs` before trusting anything here.

AgentLight treats this file as an external, read-mostly contract. Keep the port
in lockstep with the source rather than guessing.

## File locations

| Path                                  | Purpose                              |
|---------------------------------------|--------------------------------------|
| `~/.claude/clawlight/state.json`      | Session status, written by hooks.    |
| `~/.claude/clawlight/.state.lock`     | Write mutex (see below).             |
| `~/.claude/clawlight/config.json`     | clawlight's own settings (not ours). |

In a container running as the `opencode` user, the state directory is
`/home/opencode/.claude/clawlight/`. `~` is resolved with `dirs::home_dir()`
(`state.rs:154`).

## Shape

```json
{
  "sessions": {
    "ses_f70516964ffeHnea6LfdkaM0GH": {
      "status": "active",
      "last_updated": "2026-09-11T09:01:41Z",
      "project_path": "/",
      "notification_type": null,
      "name": "Caveman terse response mode skill",
      "terminal": {},
      "harness": "opencode"
    }
  }
}
```

`HookState` (`state.rs:104`) is a single `sessions` map keyed by session id.
`SessionStatus` (`state.rs:30`) fields:

| Field               | Type              | Notes                                                        |
|---------------------|-------------------|--------------------------------------------------------------|
| `status`            | enum, snake_case  | See below. Required.                                         |
| `last_updated`      | string (RFC 3339) | Written on every event. Used for the staleness backstop.     |
| `project_path`      | string \| null    | Working directory reported by the hook.                      |
| `notification_type` | string \| null    | Present on `NeedsHelp`; `null` otherwise.                    |
| `name`              | string \| null    | clawlight LLM/auto-generated title.                          |
| `terminal`          | object \| absent  | Terminal identity for click-to-focus; may be `{}`.           |
| `harness`           | string \| absent  | Absent = Claude Code; `"opencode"`, `"codex"`, `"copilot"`, … |

Unknown fields must be preserved on write. Missing optional fields must not
crash the parser.

## Status values

JSON is `snake_case` (`#[serde(rename_all = "snake_case")]`, `state.rs:12`).
The human display string differs (`state.rs:22-27`):

| JSON value   | Display      | TUI symbol | Color  |
|--------------|--------------|------------|--------|
| `active`     | `working`    | `●`        | green  |
| `inactive`   | `paused`     | `●`        | yellow |
| `needs_help` | `needs help` | `●`        | red    |
| `done`       | `done`       | `○`        | dim    |

`ui.rs:132-140`. Palette (`ui.rs:14-18`): green `#98c379`, yellow `#e5c07b`,
red `#e06c75`, cyan `#56b6c2`, dim `#5c6370`.

## How a status is produced

Hooks and the opencode plugin emit normalized verbs; the binary maps them
(`hook.rs:239-243`):

| Verb                      | Status       |
|---------------------------|--------------|
| `working`, `resumed`      | `active`     |
| `idle`                    | `inactive`   |
| `needs_input`             | `needs_help` |
| `ended`                   | `done`       |
| `title`                   | no status change (name only)                                |
| `reconnected`             | sweep: this harness's stale `active`/`inactive` → `done`    |

Claude Code hook events map in-process (`hook.rs:85-97`): `SessionStart` /
`UserPromptSubmit` / `PreToolUse` → `active`; `Stop` → `inactive`;
`Notification` → `needs_help` (except `idle_prompt`); `SessionEnd` → `done`.

## Aggregate rule

`aggregate` (`state.rs:124-152`) counts non-`done` sessions:

- any `needs_help` → **Red**
- else, `yellow_mode == any_inactive` (default): any `inactive` → **Yellow**
- else, `yellow_mode == active_wins`: any `active` → **Green**, otherwise (all
  inactive) → **Yellow**
- no live sessions (all `done` / empty) → **None** (gray)

## Reaping and staleness

`read_hook_state` (`state.rs:162`) copies the state, then `reap_ended_sessions`
(`state.rs:194`) downgrades to `done` in memory only — it never writes the file:

1. If `terminal.owner_pid` is set and the process is dead → `done`.
2. For a live PID, never stale-reap (authoritative).
3. With no captured PID, `last_updated` older than `STALE_AFTER_HOURS = 24`
   (`state.rs:9`) → `done`.

**Host-visible consequence:** `owner_pid` is namespaced to the container. A host
reader cannot run the liveness check, so it should apply only the 24h rule and
otherwise trust the file. Do not attempt to interpret `owner_pid` as a host PID.

## The `.state.lock` write mutex

`acquire_state_lock` (`state.rs:237-251`):

- Opens `.state.lock` next to `state.json` with create + write,
  `truncate(false)` (the file stays empty).
- Takes a blocking exclusive lock via `fs4` (`flock(2)` on Unix,
  `LockFileEx` on Windows).
- Released when the returned `File` drops.
- Returns `None` if the lock cannot be acquired; callers proceed unlocked.
- The file is never deleted; `clawlight uninstall` removes the whole directory.

Who uses it:

- `clear_session` (`state.rs:215-229`) — the TUI's `x` remove.
- `hook::update_state` — every hook/event write.

Who does not: `read_hook_state`. Readers rely on the atomic write below.

AgentLight takes this lock for its own read-modify-write (Remove), and skips it
for plain reads. Caveats:

- It is advisory: only cooperating writers honor it.
- OS file locks are not guaranteed across Docker bind mounts or network shares;
  treat the lock as best-effort there and always keep the atomic write.

## Atomic writes

`write_state_atomic` (`state.rs:256-265`):

1. Serialize to `.state.<pid>.tmp` in the same directory.
2. `rename` the temp file onto `state.json`.

A concurrent reader therefore always sees either the old or the new complete
file, never a partial one. AgentLight uses the same temp+rename strategy and
must never write when the read failed to parse (clawlight returns `Unreadable`
and the caller skips the write).

## Display merge and ordering

`merge_sessions` (`session.rs:119`) merges hook-state entries with Claude
`sessions-index.json` entries, then:

- Name priority: hook-state `name` > `customTitle` > `summary` > truncated
  `first_prompt` > `Session <8-char id prefix>` (`session.rs:133-145`).
- Sort: `needs_help` → `active` → `inactive` → `done`, then `modified` /
  `last_updated` descending (`session.rs:193-201`).
- Retain only the 5 most recent `done` rows (`session.rs:205-212`).
- Truncate by **chars**, not bytes: byte-slicing arbitrary UTF-8 names panics
  (`session.rs:80-97`).

AgentLight only needs the hook-state half (it does not read Claude's
`sessions-index.json`), but preserves the name priority, sort, retention, and
char-safe truncation.

## Upstream references

- `src/state.rs` — types, aggregate, reap, lock, atomic write, clear.
- `src/session.rs` — merge, naming, sort, retention, truncation.
- `src/hook.rs` — verb → status mapping, reconnect sweep.
- `src/config.rs` — `YellowMode` (`any_inactive` / `active_wins`).
- `src/ui.rs` — palette and status labels.
- `assets/opencode-plugin.js` — opencode event → verb translation.
