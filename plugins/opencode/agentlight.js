// AgentLight opencode plugin (reference implementation).
//
// Forwards opencode session state to an AgentLight hub running in events mode
// (`AGENTLIGHT_SOURCE=events`). It is an event-bus observer: it never changes
// opencode behavior, it only reports.
//
// Install: copy this file to `.opencode/plugins/agentlight.js` (project) or
// `~/.config/opencode/plugins/agentlight.js` (global). opencode loads any
// `*.js`/`*.ts` in those directories at startup.
//
// Environment (read from the opencode process):
//   AGENTLIGHT_HUB_URL       Hub base URL. Default http://127.0.0.1:8787
//   AGENTLIGHT_TOKEN         Bearer token, if the hub requires auth. Never logged.
//   AGENTLIGHT_HEARTBEAT_MS  Heartbeat snapshot interval in ms. Default 30000.
//                            `0` disables the periodic snapshot.
//   AGENTLIGHT_AUTOSTART_BIN Optional absolute path to the `agentlight-server`
//                            binary. Opt-in: unset/empty means this plugin never
//                            probes or starts anything and event reporting is
//                            unchanged. When set, the plugin probes
//                            `GET ${hub}/healthz` once at startup; if the hub is
//                            healthy it does nothing. If it is unreachable, the
//                            plugin starts the binary **detached** (stdio
//                            ignored, `unref()`ed) and polls `/healthz` for a
//                            few seconds, logging a warning if it never comes up.
//                            The plugin does not own the process: it never stops
//                            the server on exit, and a failed start is logged,
//                            never thrown.
//
// Mapping (opencode -> AgentLight `status`):
//   session.status busy / retry      -> active      (working)
//   session.status idle, session.idle
//     main session                   -> inactive    (paused, waiting on you)
//     subagent (has a parentID)      -> done        (it has no more turns)
//   permission.asked / .updated      -> needs_help  (waiting on you)
//   permission.replied               -> active      (you answered, it resumes)
//   session.error                    -> needs_help
//   session.deleted                  -> done
//
// A finished subagent does not emit `session.deleted`; it only goes idle, so
// idle on a tool-spawned child session is reported as `done`. An idle event
// while a permission is pending is not "finished": it stays `needs_help`.
//
// Status is forwarded with a short per-session coalescing window so bursts of
// events collapse into the latest state.
//
// On top of those upserts the plugin sends a heartbeat **snapshot**:
// `{ "mode": "snapshot", "events": [...] }`. A snapshot upserts the batch and
// then prunes any hub-side session absent from it, so the hub can recover
// last-known state after either side restarts. One snapshot is sent shortly
// after startup, then every `AGENTLIGHT_HEARTBEAT_MS` (default 30000; `0`
// disables the interval). The snapshot carries the live set: every session
// that is not `done`, plus the newest five `done` sessions by update time.
//
// NOTE: this targets the documented plugin API and the event names in
// `@opencode-ai/sdk` at the time of writing, and has been validated against a
// live opencode build. Other versions may rename events, so adapt the `switch`
// in `handleEvent` below if an event name or payload differs.

import { spawn as nodeSpawn } from "node:child_process";

const DEFAULT_HUB_URL = "http://127.0.0.1:8787";
const COALESCE_MS = 150;
const DEFAULT_HEARTBEAT_MS = 30000;
const MAX_DONE_SESSIONS = 5;
const HARNESS = "opencode";
const DEFAULT_HEALTH_PROBE_TIMEOUT_MS = 1000;
const DEFAULT_HEALTH_POLL_INTERVAL_MS = 250;
const DEFAULT_HEALTH_POLL_TIMEOUT_MS = 5000;

export const AgentLightPlugin = async (
  { directory, client },
  { spawn = nodeSpawn, fetch = globalThis.fetch, timing = {} } = {},
) => {
  const hubUrl = (process.env.AGENTLIGHT_HUB_URL || DEFAULT_HUB_URL).replace(/\/+$/, "");
  const token = process.env.AGENTLIGHT_TOKEN || "";
  const ingestUrl = `${hubUrl}/api/v1/ingest`;
  const parsedHeartbeat = Number.parseInt(process.env.AGENTLIGHT_HEARTBEAT_MS ?? "", 10);
  const heartbeatMs = Number.isNaN(parsedHeartbeat)
    ? DEFAULT_HEARTBEAT_MS
    : Math.max(0, parsedHeartbeat);

  const autostartBin = (process.env.AGENTLIGHT_AUTOSTART_BIN || "").trim();
  const healthProbeTimeoutMs = timing.probeTimeoutMs ?? DEFAULT_HEALTH_PROBE_TIMEOUT_MS;
  const healthPollIntervalMs = timing.pollIntervalMs ?? DEFAULT_HEALTH_POLL_INTERVAL_MS;
  const healthPollTimeoutMs = timing.pollTimeoutMs ?? DEFAULT_HEALTH_POLL_TIMEOUT_MS;

  // Capture fetch at load so a heartbeat from this instance keeps using the
  // transport it started with (and cannot reach the network after a test or
  // sidecar swaps globals).
  const postFetch = typeof fetch === "function" ? fetch.bind(globalThis) : fetch;

  // sessionID -> { name, projectPath, parentID, status, updatedAt }
  const sessions = new Map();
  // sessionID -> { event, timer }
  const pending = new Map();
  // sessionID -> waiting on a permission reply; idle must not mark it done.
  const awaitingPermission = new Set();

  const log = async (level, message, extra) => {
    try {
      await client.app.log({
        body: { service: "agentlight", level, message, extra },
      });
    } catch {
      if (level === "error") console.error(`agentlight: ${message}`, extra || "");
    }
  };

  const now = () => new Date().toISOString();

  const sleep = (ms) =>
    new Promise((resolve) => {
      const timer = setTimeout(resolve, ms);
      // Allow opencode to exit without waiting on the poll loop.
      if (timer && typeof timer.unref === "function") timer.unref();
    });

  const probeHealth = async () => {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), healthProbeTimeoutMs);
    if (timer && typeof timer.unref === "function") timer.unref();
    try {
      const response = await postFetch(`${hubUrl}/healthz`, {
        method: "GET",
        signal: controller.signal,
      });
      return Boolean(response && response.ok);
    } catch {
      return false;
    } finally {
      clearTimeout(timer);
    }
  };

  // Opt-in hub autostart. Unset `AGENTLIGHT_AUTOSTART_BIN` makes this a no-op,
  // so the plugin's event reporting is unchanged. The child is spawned detached
  // and unref'd: the plugin never waits on it and never stops it on exit.
  const ensureHubRunning = async () => {
    if (!autostartBin) return;
    if (await probeHealth()) return;
    try {
      const child = spawn(autostartBin, [], {
        detached: true,
        stdio: "ignore",
        env: process.env,
      });
      if (child && typeof child.unref === "function") child.unref();
    } catch (error) {
      await log("warn", "could not start AgentLight hub", {
        bin: autostartBin,
        error: String(error && error.message ? error.message : error),
      });
      return;
    }

    const deadline = Date.now() + healthPollTimeoutMs;
    while (Date.now() < deadline) {
      await sleep(healthPollIntervalMs);
      if (await probeHealth()) return;
    }
    await log("warn", "AgentLight hub did not become healthy after autostart", {
      bin: autostartBin,
      waited_ms: healthPollTimeoutMs,
    });
  };

  // Fire-and-forget: startup must not block on the hub coming up.
  ensureHubRunning().catch(() => {});

  const buildEvent = (sessionID, status) => {
    const known = sessions.get(sessionID) || {};
    return {
      session_id: sessionID,
      status,
      name: known.name || null,
      project_path: known.projectPath || directory || null,
      harness: HARNESS,
      last_updated: known.updatedAt || now(),
    };
  };

  const flush = async (sessionID) => {
    const entry = pending.get(sessionID);
    if (!entry) return;
    pending.delete(sessionID);
    const body = JSON.stringify({ events: [entry.event] });

    const headers = { "Content-Type": "application/json" };
    if (token) headers.Authorization = `Bearer ${token}`;

    try {
      const response = await postFetch(ingestUrl, { method: "POST", headers, body });
      if (!response.ok) {
        const text = await response.text().catch(() => "");
        await log("warn", `hub rejected event for ${sessionID}`, {
          status: response.status,
          body: text.slice(0, 200),
        });
      }
    } catch (error) {
      await log("error", `could not reach hub at ${hubUrl}`, {
        error: String(error && error.message ? error.message : error),
      });
    }
  };

  // Record the last-known status and when it was seen, so a heartbeat can sort
  // recent `done` sessions and resend what the hub may have missed.
  const touch = (sessionID, status) => {
    const known = sessions.get(sessionID) || {};
    sessions.set(sessionID, { ...known, status, updatedAt: now() });
  };

  const report = (sessionID, status) => {
    if (!sessionID) return;
    touch(sessionID, status);
    const event = buildEvent(sessionID, status);
    const existing = pending.get(sessionID);
    if (existing) clearTimeout(existing.timer);
    const timer = setTimeout(() => {
      flush(sessionID);
    }, COALESCE_MS);
    // Allow opencode to exit without waiting on the timer.
    if (timer && typeof timer.unref === "function") timer.unref();
    pending.set(sessionID, { event, timer });
  };

  const remember = (info) => {
    if (!info || !info.id) return;
    const known = sessions.get(info.id) || {};
    sessions.set(info.id, {
      ...known,
      name: info.title || null,
      projectPath: info.directory || null,
      parentID: info.parentID || null,
    });
  };

  const snapshotEvents = () => {
    const live = [];
    const done = [];
    for (const [sessionID, info] of sessions) {
      if (info.status === "done") done.push([sessionID, info]);
      else live.push([sessionID, info]);
    }
    // Newest `done` first; ISO-8601 strings sort lexicographically.
    done.sort((a, b) => String(b[1].updatedAt || "").localeCompare(String(a[1].updatedAt || "")));
    return [...live, ...done.slice(0, MAX_DONE_SESSIONS)].map(([sessionID, info]) =>
      buildEvent(sessionID, info.status || "inactive"),
    );
  };

  const sendSnapshot = async () => {
    const body = JSON.stringify({ mode: "snapshot", events: snapshotEvents() });

    const headers = { "Content-Type": "application/json" };
    if (token) headers.Authorization = `Bearer ${token}`;

    try {
      const response = await postFetch(ingestUrl, { method: "POST", headers, body });
      if (!response.ok) {
        const text = await response.text().catch(() => "");
        await log("warn", "hub rejected snapshot", {
          status: response.status,
          body: text.slice(0, 200),
        });
      }
    } catch (error) {
      await log("error", `could not reach hub at ${hubUrl}`, {
        error: String(error && error.message ? error.message : error),
      });
    }
  };

  // A session created by a tool (a subagent) carries a `parentID`. It runs to
  // completion without waiting on the user, so idle means finished. Fall back
  // to asking opencode when an event arrives before `session.created`.
  const isSubagent = async (sessionID) => {
    const known = sessions.get(sessionID);
    if (known) return Boolean(known.parentID);
    try {
      const result = await client.session.get({ path: { id: sessionID } });
      const info = result && result.data;
      if (info) {
        remember(info);
        return Boolean(info.parentID);
      }
    } catch (error) {
      await log("warn", `could not read session ${sessionID}`, {
        error: String(error && error.message ? error.message : error),
      });
    }
    return false;
  };

  const reportIdle = async (sessionID) => {
    if (!sessionID) return;
    if (awaitingPermission.has(sessionID)) {
      report(sessionID, "needs_help");
      return;
    }
    report(sessionID, (await isSubagent(sessionID)) ? "done" : "inactive");
  };

  const handleEvent = async (event) => {
    const props = event.properties || {};
    switch (event.type) {
      case "session.created":
      case "session.updated":
        remember(props.info);
        break;
      case "session.deleted":
        if (props.info) {
          remember(props.info);
          awaitingPermission.delete(props.info.id);
          report(props.info.id, "done");
        } else if (props.sessionID) {
          awaitingPermission.delete(props.sessionID);
          report(props.sessionID, "done");
        }
        break;
      case "session.status": {
        const status = props.status && props.status.type;
        if (status === "idle") {
          await reportIdle(props.sessionID);
        } else {
          report(props.sessionID, "active");
        }
        break;
      }
      case "session.idle":
        await reportIdle(props.sessionID);
        break;
      case "session.error":
        report(props.sessionID, "needs_help");
        break;
      // opencode's SDK has used both names for the "waiting for approval" event.
      case "permission.asked":
      case "permission.updated":
        if (props.sessionID) awaitingPermission.add(props.sessionID);
        report(props.sessionID, "needs_help");
        break;
      case "permission.replied":
        if (props.sessionID) awaitingPermission.delete(props.sessionID);
        report(props.sessionID, "active");
        break;
      default:
        break;
    }
  };

  // One snapshot shortly after startup, so a hub that restarted (or missed our
  // events while it was down) resyncs to the current live set. Deferred a tick
  // so sessions created during startup land in the first snapshot.
  setTimeout(() => {
    sendSnapshot();
  }, 0);

  if (heartbeatMs > 0) {
    const heartbeat = setInterval(() => {
      sendSnapshot();
    }, heartbeatMs);
    // Allow opencode to exit without waiting on the heartbeat.
    if (heartbeat && typeof heartbeat.unref === "function") heartbeat.unref();
  }

  return {
    event: async ({ event }) => {
      try {
        await handleEvent(event);
      } catch (error) {
        await log("error", "failed to handle opencode event", {
          type: event && event.type,
          error: String(error && error.message ? error.message : error),
        });
      }
    },
    "tool.execute.before": async ({ sessionID }) => {
      report(sessionID, "active");
    },
    "tool.execute.after": async ({ sessionID }) => {
      report(sessionID, "active");
    },
  };
};
