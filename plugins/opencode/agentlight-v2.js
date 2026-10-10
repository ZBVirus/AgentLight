// AgentLight opencode plugin, OpenCode V2 edition.
//
// The V2 counterpart of `agentlight.js` (which targets the OpenCode V1 plugin
// API). V1 plugin implementations do not run in V2: V2 loads a default export
// with an `id` and a `setup(ctx)` function, subscribes to events through
// `ctx.event.subscribe()`, and reads event payloads from `event.data` (V1 used
// `event.properties`). This file keeps the same reporting behavior and hub
// contract as the V1 plugin; the mapping table below is the only difference.
//
// Install (OpenCode V2): copy this file to `.opencode/plugins/agentlight.js`
// (project) or `~/.config/opencode/plugins/agentlight.js` (global). V2 discovers
// direct `.js`/`.ts` files and package directories under `.opencode/plugins/`.
// Do not also copy the V1 `agentlight.js` into the same directory: V2 logs a
// load warning for it because it has no V2 default export. Keep one or the
// other.
//
// The default export is the same shape `Plugin.define({ id, setup })` from
// `@opencode/plugin` produces (that helper is an identity function for Promise
// plugins). Inlining it keeps this file dependency-free for a hand-copy install
// and lets the Node test harness import and drive it without an OpenCode
// runtime.
//
// Environment (read from the opencode process):
//   AGENTLIGHT_HUB_URL       Hub base URL. Default http://127.0.0.1:8787
//   AGENTLIGHT_TOKEN         Bearer token, if the hub requires auth. Never logged.
//   AGENTLIGHT_PRODUCER      Optional stable producer id. Defaults to
//                            `opencode:<hostname>:<directory>`, which is stable
//                            per project and distinguishes hosts. Snapshots are
//                            producer-scoped, so one instance's snapshot prunes
//                            only its own sessions, never another instance's.
//   AGENTLIGHT_HEARTBEAT_MS  Heartbeat snapshot interval in ms. Default 30000.
//                            `0` disables the periodic snapshot.
//   AGENTLIGHT_SESSION_URL_TEMPLATE
//                            Best-effort session deep link. Defaults to
//                            `http://localhost:4096/session/{id}` (opencode's
//                            usual web address) so the desktop's "Open" action
//                            works with no setup. Every reported event and
//                            heartbeat snapshot entry carries `url` with every
//                            `{id}` replaced by the URL-encoded session id. Set
//                            an explicit empty value to disable the link, or a
//                            custom template to override it. A native app cannot
//                            focus an exact browser window or tab, so the link is
//                            best-effort: the browser decides whether to reuse a
//                            tab or open a new one.
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
// Mapping (OpenCode V2 -> AgentLight `status`):
//   session.status busy / retry           -> active      (working)
//   session.status idle, session.idle
//     main session                        -> inactive    (paused, waiting on you)
//     subagent (has a parentID)           -> done        (it has no more turns)
//   session.execution.started             -> active
//   session.execution.failed              -> needs_help
//   session.execution.succeeded/          -> idle path   (done for a subagent,
//     .interrupted                                        inactive for a main)
//   permission.asked                      -> needs_help  (waiting on you)
//   permission.replied                    -> active      (you answered, it resumes)
//   form.created                          -> needs_help  (opencode asks a question)
//   form.replied                          -> active
//   form.cancelled                        -> idle path
//   session.deleted                       -> done
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
// then prunes the hub-side sessions belonging to the same `producer` that are
// absent from it, so the hub can recover last-known state after either side
// restarts without one instance evicting another's sessions. One snapshot is
// sent shortly after startup, then every `AGENTLIGHT_HEARTBEAT_MS` (default
// 30000; `0` disables the interval). An empty live set is never sent, so a
// starting or idle instance cannot wipe the hub. The snapshot carries the live
// set: every session that is not `done`, plus the newest five `done` sessions
// by update time.

import { spawn as nodeSpawn } from "node:child_process";
import os from "node:os";

export const DEFAULT_HUB_URL = "http://127.0.0.1:8787";
// Bumped when an operator must re-copy the plugin to get a behavior fix. The
// copy is manual, so a stale file is a common failure mode; the startup log
// below makes the installed version visible in opencode's logs.
export const PLUGIN_VERSION = "0.6.0-dev";
const COALESCE_MS = 150;
const DEFAULT_HEARTBEAT_MS = 30000;
const MAX_DONE_SESSIONS = 5;
const HARNESS = "opencode";
export const DEFAULT_HEALTH_PROBE_TIMEOUT_MS = 1000;
export const DEFAULT_HEALTH_POLL_INTERVAL_MS = 250;
export const DEFAULT_HEALTH_POLL_TIMEOUT_MS = 5000;

// Same shape as `Plugin.define` from `@opencode/plugin`. Kept local so the file
// has no import that only an OpenCode runtime can resolve.
const define = (plugin) => plugin;

// Build the engine for one loaded plugin instance. All OpenCode access is
// injected so the same code runs under `setup(ctx)` and under the Node test
// harness. Returns the pieces the caller wires into OpenCode (`report` for the
// tool hooks, `dispose` for cleanup) plus `handleEvent` for tests.
export const createAgentLightV2 = async ({
  directory,
  getSession,
  subscribe,
  log,
  opencodeVersion,
  spawn = nodeSpawn,
  fetch: fetchImpl = globalThis.fetch,
  timing = {},
  env = process.env,
}) => {
  const hubUrl = (env.AGENTLIGHT_HUB_URL || DEFAULT_HUB_URL).replace(/\/+$/, "");
  const token = env.AGENTLIGHT_TOKEN || "";
  // Stable per project and host, so each opencode instance's snapshot prunes
  // only its own sessions.
  const producer =
    (env.AGENTLIGHT_PRODUCER || "").trim() || `opencode:${os.hostname()}:${directory}`;
  const ingestUrl = `${hubUrl}/api/v1/ingest`;
  const parsedHeartbeat = Number.parseInt(env.AGENTLIGHT_HEARTBEAT_MS ?? "", 10);
  const heartbeatMs = Number.isNaN(parsedHeartbeat)
    ? DEFAULT_HEARTBEAT_MS
    : Math.max(0, parsedHeartbeat);

  const autostartBin = (env.AGENTLIGHT_AUTOSTART_BIN || "").trim();
  // Best-effort session deep link. Unset uses opencode's usual web address so
  // the desktop's "Open" action works with no setup; set an explicit empty
  // value to disable it, or a custom template (with `{id}`) to override it.
  const sessionUrlEnv = env.AGENTLIGHT_SESSION_URL_TEMPLATE;
  const sessionUrlTemplate =
    sessionUrlEnv === undefined
      ? "http://localhost:4096/session/{id}"
      : String(sessionUrlEnv).trim();
  const healthProbeTimeoutMs = timing.probeTimeoutMs ?? DEFAULT_HEALTH_PROBE_TIMEOUT_MS;
  const healthPollIntervalMs = timing.pollIntervalMs ?? DEFAULT_HEALTH_POLL_INTERVAL_MS;
  const healthPollTimeoutMs = timing.pollTimeoutMs ?? DEFAULT_HEALTH_POLL_TIMEOUT_MS;

  // Opt-in event tracing: log each event `type` and the keys of its payload, so
  // the shape a given opencode build emits can be confirmed from the logs.
  const debugEvents = /^(1|true|yes|on)$/i.test((env.AGENTLIGHT_DEBUG || "").trim());

  // Capture fetch so a heartbeat from this instance keeps using the transport it
  // started with (and cannot reach the network after a test or sidecar swaps
  // globals).
  const postFetch = typeof fetchImpl === "function" ? fetchImpl.bind(globalThis) : fetchImpl;

  // sessionID -> { name, projectPath, parentID, status, updatedAt }
  const sessions = new Map();
  // sessionID -> { event, timer }
  const pending = new Map();
  // sessionID -> waiting on a permission reply; idle must not mark it done.
  const awaitingPermission = new Set();

  // One line to confirm which copy is loaded (the file is installed by hand).
  log("info", `plugin ${PLUGIN_VERSION} started (opencode v2)`, {
    producer,
    hub: hubUrl,
    opencode: opencodeVersion,
    heartbeat_ms: heartbeatMs,
    session_url: sessionUrlTemplate || "disabled",
    autostart: autostartBin ? "enabled" : "disabled",
    debug: debugEvents ? "on" : "off",
  });

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
        env,
      });
      if (child && typeof child.unref === "function") child.unref();
    } catch (error) {
      log("warn", "could not start AgentLight hub", {
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
    log("warn", "AgentLight hub did not become healthy after autostart", {
      bin: autostartBin,
      waited_ms: healthPollTimeoutMs,
    });
  };

  // Fire-and-forget: startup must not block on the hub coming up.
  ensureHubRunning().catch(() => {});

  const buildSessionUrl = (sessionID) => {
    if (!sessionUrlTemplate || !sessionID) return null;
    return sessionUrlTemplate.replace(/\{id\}/g, encodeURIComponent(String(sessionID)));
  };

  const buildEvent = (sessionID, status) => {
    const known = sessions.get(sessionID) || {};
    return {
      session_id: sessionID,
      status,
      name: known.name || null,
      project_path: known.projectPath || directory || null,
      url: buildSessionUrl(sessionID),
      harness: HARNESS,
      last_updated: known.updatedAt || now(),
      producer,
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
        log("warn", `hub rejected event for ${sessionID}`, {
          status: response.status,
          body: text.slice(0, 200),
        });
      }
    } catch (error) {
      log("error", `could not reach hub at ${hubUrl}`, {
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

  // V2 `session.created` carries the fields under `data`; `session.get` returns
  // the same `Session.Info` fields at the top level. Accept either a
  // `{ sessionID }` payload or a `{ id }` info object. A missing or empty title
  // must not erase a name already learned (titles are generated after creation).
  const remember = (info) => {
    if (!info) return;
    const id = info.sessionID || info.id;
    if (!id) return;
    const known = sessions.get(id) || {};
    const location = info.location || {};
    const title = typeof info.title === "string" && info.title.trim() ? info.title : null;
    sessions.set(id, {
      ...known,
      name: title || known.name || null,
      projectPath:
        (location && location.directory) || info.directory || known.projectPath || null,
      parentID: info.parentID !== undefined ? info.parentID : known.parentID || null,
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
    const events = snapshotEvents();
    // Never POST an empty snapshot: a starting or idle instance must not wipe
    // the hub's live set.
    if (events.length === 0) return;
    const body = JSON.stringify({ mode: "snapshot", events });

    const headers = { "Content-Type": "application/json" };
    if (token) headers.Authorization = `Bearer ${token}`;

    try {
      const response = await postFetch(ingestUrl, { method: "POST", headers, body });
      if (!response.ok) {
        const text = await response.text().catch(() => "");
        log("warn", "hub rejected snapshot", {
          status: response.status,
          body: text.slice(0, 200),
        });
      }
    } catch (error) {
      log("error", `could not reach hub at ${hubUrl}`, {
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
      const result = await getSession(sessionID);
      const info = result && result.data ? result.data : result;
      if (info) {
        remember({ ...info, sessionID: info.id || sessionID });
        return Boolean(info.parentID);
      }
    } catch (error) {
      log("warn", `could not read session ${sessionID}`, {
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
    if (!event || !event.type) return;
    // V2 nests the payload under `data`; `properties` is kept as a fallback for
    // a build that still emits the V1 shape.
    const props = event.data || event.properties || {};
    if (debugEvents) {
      log("info", `event ${event.type}`, {
        event_keys: Object.keys(event),
        payload_keys: Object.keys(props),
      });
    }
    switch (event.type) {
      case "session.created":
      case "session.updated":
        remember(props.info || props);
        break;
      case "session.renamed":
        if (props.sessionID && props.title !== undefined) {
          const known = sessions.get(props.sessionID) || {};
          sessions.set(props.sessionID, { ...known, name: props.title });
        }
        break;
      case "session.deleted":
        if (props.sessionID) {
          awaitingPermission.delete(props.sessionID);
          report(props.sessionID, "done");
        } else if (props.info && props.info.id) {
          remember(props.info);
          awaitingPermission.delete(props.info.id);
          report(props.info.id, "done");
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
      case "session.execution.started":
        report(props.sessionID, "active");
        break;
      case "session.execution.failed":
        report(props.sessionID, "needs_help");
        break;
      case "session.execution.succeeded":
      case "session.execution.interrupted":
        await reportIdle(props.sessionID);
        break;
      // V1 name for a session-level error; kept for payload compatibility.
      case "session.error":
        report(props.sessionID, "needs_help");
        break;
      case "permission.asked":
      case "permission.updated":
        if (props.sessionID) awaitingPermission.add(props.sessionID);
        report(props.sessionID, "needs_help");
        break;
      case "permission.replied":
        if (props.sessionID) awaitingPermission.delete(props.sessionID);
        report(props.sessionID, "active");
        break;
      // A V2 interactive form is opencode asking the user a question. `created`
      // waits on the user, `replied` resumes, `cancelled` falls back to idle.
      case "form.created": {
        const sid = props.sessionID || (props.form && props.form.sessionID);
        if (sid) awaitingPermission.add(sid);
        report(sid, "needs_help");
        break;
      }
      case "form.replied":
        if (props.sessionID) awaitingPermission.delete(props.sessionID);
        report(props.sessionID, "active");
        break;
      case "form.cancelled":
        if (props.sessionID) awaitingPermission.delete(props.sessionID);
        await reportIdle(props.sessionID);
        break;
      default:
        break;
    }
  };

  // One snapshot shortly after startup, so a hub that restarted (or missed our
  // events while it was down) resyncs to the current live set. Deferred a tick
  // so sessions created during startup land in the first snapshot.
  const startupTimer = setTimeout(() => {
    sendSnapshot();
  }, 0);
  if (startupTimer && typeof startupTimer.unref === "function") startupTimer.unref();

  let heartbeat = null;
  if (heartbeatMs > 0) {
    heartbeat = setInterval(() => {
      sendSnapshot();
    }, heartbeatMs);
    // Allow opencode to exit without waiting on the heartbeat.
    if (heartbeat && typeof heartbeat.unref === "function") heartbeat.unref();
  }

  // V2 delivers the public server event stream as an async iterable. Consume it
  // in the background and abort it from `dispose` on unload.
  const controller = new AbortController();
  const subscription = (async () => {
    const stream = subscribe({ signal: controller.signal });
    for await (const event of stream) {
      try {
        await handleEvent(event);
      } catch (error) {
        log("error", "failed to handle opencode event", {
          type: event && event.type,
          error: String(error && error.message ? error.message : error),
        });
      }
    }
  })();
  subscription.catch((error) => {
    if (!controller.signal.aborted) {
      log("error", "event subscription ended", {
        error: String(error && error.message ? error.message : error),
      });
    }
  });

  const dispose = () => {
    controller.abort();
    if (heartbeat) clearInterval(heartbeat);
    clearTimeout(startupTimer);
    for (const entry of pending.values()) clearTimeout(entry.timer);
    pending.clear();
  };

  return { report, handleEvent, snapshotEvents, dispose };
};

export default define({
  id: "agentlight",
  async setup(ctx) {
    const log = (level, message, extra) => {
      const detail = extra ? ` ${JSON.stringify(extra)}` : "";
      const line = `agentlight: ${message}${detail}`;
      if (level === "error" || level === "warn") console.error(line);
      else console.log(line);
    };

    const instance = await createAgentLightV2({
      directory: ctx.location && ctx.location.directory,
      opencodeVersion: ctx.app && ctx.app.version,
      getSession: (sessionID) => ctx.session.get({ sessionID }),
      subscribe: (options) => ctx.event.subscribe(options),
      log,
    });

    // Tool execution is a strong "the session is working" signal. V2 registers
    // each hook on the tool domain instead of returning a string-keyed map.
    await ctx.tool.hook("execute.before", (event) => {
      instance.report(event && event.sessionID, "active");
    });
    await ctx.tool.hook("execute.after", (event) => {
      instance.report(event && event.sessionID, "active");
    });

    // Cleanup runs when the plugin unloads (reload or shutdown).
    return () => instance.dispose();
  },
});
