// Tests for the opencode V2 plugin's opencode-event -> AgentLight-status
// mapping. The V2 plugin subscribes to `ctx.event.subscribe()` and reads event
// payloads from `event.data`, so these drive the real default export with a
// stub context instead of a stub `event` callback.
//
// Run with:
//   node --test tests/plugin/

import test from "node:test";
import assert from "node:assert/strict";
import os from "node:os";

import agentlightV2, { createAgentLightV2 } from "../../plugins/opencode/agentlight-v2.js";

const COALESCE_WAIT_MS = 300;

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function waitFor(predicate, { timeout = 1000, interval = 5 } = {}) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    if (predicate()) return;
    await sleep(interval);
  }
  throw new Error("waitFor timed out");
}

// A controllable async iterable standing in for `ctx.event.subscribe`.
function eventStream() {
  const queue = [];
  const waiters = [];
  let closed = false;
  const wake = () => {
    while (waiters.length) waiters.shift()();
  };
  return {
    push(event) {
      queue.push(event);
      wake();
    },
    close() {
      closed = true;
      wake();
    },
    subscribe({ signal } = {}) {
      return (async function* () {
        while (true) {
          if (signal && signal.aborted) return;
          if (closed) return;
          if (queue.length) {
            yield queue.shift();
            continue;
          }
          await new Promise((resolve) => {
            waiters.push(resolve);
            if (signal && typeof signal.addEventListener === "function") {
              signal.addEventListener("abort", () => resolve(), { once: true });
            }
          });
        }
      })();
    },
  };
}

function trackingClient() {
  const logs = [];
  return {
    logs,
    log(level, message, extra) {
      logs.push({ level, message, extra });
    },
    fetch: undefined,
  };
}

function spawnSpy() {
  const calls = [];
  let unrefCount = 0;
  const spawn = (bin, args, options) => {
    calls.push({ bin, args, options });
    return {
      pid: 4242,
      unref: () => {
        unrefCount += 1;
      },
    };
  };
  spawn.calls = calls;
  spawn.unrefCount = () => unrefCount;
  return spawn;
}

// Save an env var and restore it when the returned function runs.
function setEnv(name, value) {
  const previous = process.env[name];
  if (value === undefined) delete process.env[name];
  else process.env[name] = value;
  return () => {
    if (previous === undefined) delete process.env[name];
    else process.env[name] = previous;
  };
}

const FAST_TIMING = { probeTimeoutMs: 5, pollIntervalMs: 5, pollTimeoutMs: 20 };

// Drive the real default export through its V2 `setup(ctx)`.
async function harness(sessions = {}) {
  const calls = [];
  const stream = eventStream();
  const logs = [];
  const previousFetch = globalThis.fetch;
  globalThis.fetch = async (url, options) => {
    const body = JSON.parse(options.body);
    const events = body.events || [];
    calls.push({ url, mode: body.mode, events, event: events[0] });
    return { ok: true, status: 200, text: async () => "" };
  };

  const hooks = {};
  const ctx = {
    app: { name: "opencode", version: "2.0.25", channel: "local" },
    location: { directory: "/work/project" },
    options: {},
    session: { get: async ({ sessionID }) => sessions[sessionID] },
    event: { subscribe: (options) => stream.subscribe(options) },
    tool: {
      hook: async (name, callback) => {
        hooks[name] = callback;
        return { dispose: async () => {} };
      },
    },
  };

  const cleanup = await agentlightV2.setup(ctx);
  return {
    calls,
    hooks,
    logs,
    stream,
    event: (type, data) => stream.push({ type, data }),
    lastStatus: () => {
      for (let i = calls.length - 1; i >= 0; i--) {
        const event = calls[i].event;
        if (event && event.status) return event.status;
      }
      return undefined;
    },
    snapshots: () => calls.filter((call) => call.mode === "snapshot"),
    lastSnapshot: () => calls.filter((call) => call.mode === "snapshot").at(-1),
    restore: () => {
      if (cleanup) cleanup();
      globalThis.fetch = previousFetch;
    },
  };
}

test("a main session going idle is inactive", async (t) => {
  const h = await harness();
  t.after(h.restore);

  h.event("session.created", {
    sessionID: "main",
    title: "Main",
    location: { directory: "/work/project" },
  });
  h.event("session.status", { sessionID: "main", status: { type: "idle" } });
  await sleep(COALESCE_WAIT_MS);

  assert.equal(h.lastStatus(), "inactive");
});

test("a subagent going idle is done", async (t) => {
  const h = await harness();
  t.after(h.restore);

  h.event("session.created", {
    sessionID: "sub",
    parentID: "main",
    title: "Sub",
    location: { directory: "/work/project" },
  });
  h.event("session.status", { sessionID: "sub", status: { type: "idle" } });
  await sleep(COALESCE_WAIT_MS);

  assert.equal(h.lastStatus(), "done");
});

test("an idle session with no prior created event resolves its parent via session.get", async (t) => {
  const h = await harness({ sub: { id: "sub", parentID: "main", title: "Sub" } });
  t.after(h.restore);

  h.event("session.idle", { sessionID: "sub" });
  await sleep(COALESCE_WAIT_MS);

  assert.equal(h.lastStatus(), "done");
});

test("idle while a permission is pending stays needs_help", async (t) => {
  const h = await harness();
  t.after(h.restore);

  h.event("session.created", {
    sessionID: "sub",
    parentID: "main",
    title: "Sub",
    location: { directory: "/work/project" },
  });
  h.event("permission.asked", { sessionID: "sub", id: "p1" });
  h.event("session.idle", { sessionID: "sub" });
  await sleep(COALESCE_WAIT_MS);

  assert.equal(h.lastStatus(), "needs_help");
});

test("permission.replied returns the session to active", async (t) => {
  const h = await harness();
  t.after(h.restore);

  h.event("permission.asked", { sessionID: "main", id: "p1" });
  h.event("permission.replied", { sessionID: "main", requestID: "p1", reply: "once" });
  await sleep(COALESCE_WAIT_MS);

  assert.equal(h.lastStatus(), "active");
});

test("a deleted session is done", async (t) => {
  const h = await harness();
  t.after(h.restore);

  h.event("session.deleted", { sessionID: "main" });
  await sleep(COALESCE_WAIT_MS);

  assert.equal(h.lastStatus(), "done");
});

test("a busy session is active", async (t) => {
  const h = await harness();
  t.after(h.restore);

  h.event("session.status", { sessionID: "main", status: { type: "busy" } });
  await sleep(COALESCE_WAIT_MS);

  assert.equal(h.lastStatus(), "active");
});

test("a session execution failed event is needs_help", async (t) => {
  const h = await harness();
  t.after(h.restore);

  h.event("session.execution.failed", { sessionID: "main", error: { message: "boom" } });
  await sleep(COALESCE_WAIT_MS);

  assert.equal(h.lastStatus(), "needs_help");
});

test("a session execution started event is active", async (t) => {
  const h = await harness();
  t.after(h.restore);

  h.event("session.execution.started", { sessionID: "main" });
  await sleep(COALESCE_WAIT_MS);

  assert.equal(h.lastStatus(), "active");
});

test("a session URL template decorates reported events and snapshots", async (t) => {
  t.after(setEnv("AGENTLIGHT_SESSION_URL_TEMPLATE", "http://x/session/{id}"));
  const h = await harness();
  t.after(h.restore);

  h.event("session.created", {
    sessionID: "main",
    title: "Main",
    location: { directory: "/work/project" },
  });
  h.event("session.status", { sessionID: "main", status: { type: "busy" } });
  await sleep(COALESCE_WAIT_MS);

  const reported = h.calls.find(
    (call) => call.event && call.event.session_id === "main" && call.event.status === "active",
  );
  assert.ok(reported, "expected a reported active event");
  assert.equal(reported.event.url, "http://x/session/main");

  const snapshot = h.lastSnapshot();
  assert.ok(snapshot, "expected a startup snapshot");
  const snapEvent = snapshot.events.find((event) => event.session_id === "main");
  assert.ok(snapEvent, "snapshot should include the session");
  assert.equal(snapEvent.url, "http://x/session/main");
});

test("without an explicit template, the default session URL is used", async (t) => {
  t.after(setEnv("AGENTLIGHT_SESSION_URL_TEMPLATE", undefined));
  const h = await harness();
  t.after(h.restore);

  h.event("session.status", { sessionID: "main", status: { type: "busy" } });
  await sleep(COALESCE_WAIT_MS);

  const reported = h.calls.find(
    (call) => call.event && call.event.session_id === "main" && call.event.status === "active",
  );
  assert.ok(reported, "expected a reported active event");
  assert.equal(reported.event.url, "http://localhost:4096/session/main");
});

test("an empty session URL template disables the deep link", async (t) => {
  t.after(setEnv("AGENTLIGHT_SESSION_URL_TEMPLATE", ""));
  const h = await harness();
  t.after(h.restore);

  h.event("session.status", { sessionID: "main", status: { type: "busy" } });
  await sleep(COALESCE_WAIT_MS);

  const reported = h.calls.find(
    (call) => call.event && call.event.session_id === "main" && call.event.status === "active",
  );
  assert.ok(reported, "expected a reported active event");
  assert.equal(reported.event.url, null);
});

test("a startup snapshot is sent with mode snapshot and includes a created session", async (t) => {
  const h = await harness();
  t.after(h.restore);

  h.event("session.created", {
    sessionID: "main",
    title: "Main",
    location: { directory: "/work/project" },
  });
  await sleep(COALESCE_WAIT_MS);

  const snapshot = h.snapshots()[0];
  assert.ok(snapshot, "expected a startup snapshot");
  assert.equal(snapshot.mode, "snapshot");
  assert.ok(
    snapshot.events.some((event) => event.session_id === "main"),
    "snapshot should include the session created via session.created",
  );
});

test("a snapshot keeps every non-done session and trims done to the newest five", async (t) => {
  process.env.AGENTLIGHT_HEARTBEAT_MS = "20";
  t.after(() => {
    delete process.env.AGENTLIGHT_HEARTBEAT_MS;
  });
  const h = await harness();
  t.after(h.restore);

  h.event("session.created", {
    sessionID: "live",
    title: "Live",
    location: { directory: "/work/project" },
  });
  h.event("session.status", { sessionID: "live", status: { type: "busy" } });

  for (let i = 0; i < 7; i++) {
    h.event("session.deleted", { sessionID: `done-${i}`, info: { id: `done-${i}`, title: `Done ${i}` } });
  }

  await sleep(200);

  const snapshot = h.lastSnapshot();
  assert.ok(snapshot, "expected a heartbeat snapshot");
  const ids = snapshot.events.map((event) => event.session_id);
  assert.ok(ids.includes("live"), "non-done sessions must always be present");
  const doneIds = ids.filter((id) => id.startsWith("done-"));
  assert.equal(doneIds.length, 5, "more than five done sessions must be trimmed");
});

test("the heartbeat interval sends repeated snapshots", async (t) => {
  process.env.AGENTLIGHT_HEARTBEAT_MS = "40";
  t.after(() => {
    delete process.env.AGENTLIGHT_HEARTBEAT_MS;
  });
  const h = await harness();
  t.after(h.restore);

  h.event("session.status", { sessionID: "main", status: { type: "busy" } });
  await sleep(200);

  assert.ok(
    h.snapshots().length > 1,
    `expected more than one snapshot, got ${h.snapshots().length}`,
  );
});

test("reported events and snapshots carry the stable default producer", async (t) => {
  t.after(setEnv("AGENTLIGHT_PRODUCER", undefined));
  const h = await harness();
  t.after(h.restore);

  h.event("session.status", { sessionID: "main", status: { type: "busy" } });
  await sleep(COALESCE_WAIT_MS);

  const expected = `opencode:${os.hostname()}:/work/project`;
  const reported = h.calls.find(
    (call) => call.event && call.event.session_id === "main" && call.event.status === "active",
  );
  assert.ok(reported, "expected a reported event");
  assert.equal(reported.event.producer, expected);

  const snapshot = h.lastSnapshot();
  assert.ok(snapshot, "expected a startup snapshot");
  assert.equal(snapshot.events[0].producer, expected);
});

test("AGENTLIGHT_PRODUCER overrides the default producer", async (t) => {
  t.after(setEnv("AGENTLIGHT_PRODUCER", "custom:p1"));
  const h = await harness();
  t.after(h.restore);

  h.event("session.status", { sessionID: "main", status: { type: "busy" } });
  await sleep(COALESCE_WAIT_MS);

  const reported = h.calls.find(
    (call) => call.event && call.event.session_id === "main" && call.event.status === "active",
  );
  assert.ok(reported, "expected a reported event");
  assert.equal(reported.event.producer, "custom:p1");
});

test("an empty state does not post a snapshot, but a populated heartbeat does", async (t) => {
  process.env.AGENTLIGHT_HEARTBEAT_MS = "20";
  t.after(() => {
    delete process.env.AGENTLIGHT_HEARTBEAT_MS;
  });
  const h = await harness();
  t.after(h.restore);

  // No sessions yet: the startup snapshot and heartbeats are skipped.
  await sleep(120);
  assert.equal(h.snapshots().length, 0, "an empty instance must not POST a snapshot");

  h.event("session.status", { sessionID: "main", status: { type: "busy" } });
  await sleep(120);

  const snapshot = h.lastSnapshot();
  assert.ok(snapshot, "a non-empty heartbeat should POST a snapshot");
  assert.equal(snapshot.mode, "snapshot");
  assert.ok(snapshot.events.length > 0, "the snapshot carries the live session");
  assert.ok(
    snapshot.events.every((event) => typeof event.producer === "string" && event.producer),
    "every snapshot event carries a producer",
  );
});

test("a renamed session updates the display name in later snapshots", async (t) => {
  process.env.AGENTLIGHT_HEARTBEAT_MS = "20";
  t.after(() => {
    delete process.env.AGENTLIGHT_HEARTBEAT_MS;
  });
  const h = await harness();
  t.after(h.restore);

  h.event("session.created", {
    sessionID: "main",
    title: "Before",
    location: { directory: "/work/project" },
  });
  h.event("session.renamed", { sessionID: "main", title: "After" });
  await sleep(150);

  const snapshot = h.lastSnapshot();
  assert.ok(snapshot, "expected a heartbeat snapshot");
  const snapEvent = snapshot.events.find((event) => event.session_id === "main");
  assert.ok(snapEvent, "snapshot should include the session");
  assert.equal(snapEvent.name, "After");
});

test("a tool hook reports the session active", async (t) => {
  const h = await harness();
  t.after(h.restore);

  assert.equal(typeof h.hooks["execute.before"], "function");
  h.hooks["execute.before"]({ sessionID: "main" });
  await sleep(COALESCE_WAIT_MS);

  assert.equal(h.lastStatus(), "active");
});

test("an interactive form is needs_help, and replying resumes", async (t) => {
  const h = await harness();
  t.after(h.restore);

  h.event("form.created", { form: { id: "f1", sessionID: "main", title: "Which option?" } });
  await sleep(COALESCE_WAIT_MS);
  assert.equal(h.lastStatus(), "needs_help");

  h.event("form.replied", { id: "f1", sessionID: "main", answer: "A" });
  await sleep(COALESCE_WAIT_MS);
  assert.equal(h.lastStatus(), "active");
});

test("session.created data sets the name and project directly", async (t) => {
  process.env.AGENTLIGHT_HEARTBEAT_MS = "20";
  t.after(() => {
    delete process.env.AGENTLIGHT_HEARTBEAT_MS;
  });
  const h = await harness();
  t.after(h.restore);

  h.event("session.created", {
    sessionID: "main",
    title: "Direct title",
    location: { directory: "/work/project" },
  });
  await sleep(150);

  const event = h.lastSnapshot().events.find((e) => e.session_id === "main");
  assert.equal(event.name, "Direct title");
  assert.equal(event.project_path, "/work/project");
});

test("a later empty title does not erase a learned name", async (t) => {
  process.env.AGENTLIGHT_HEARTBEAT_MS = "20";
  t.after(() => {
    delete process.env.AGENTLIGHT_HEARTBEAT_MS;
  });
  const h = await harness();
  t.after(h.restore);

  h.event("session.created", {
    sessionID: "main",
    title: "Real name",
    location: { directory: "/work/project" },
  });
  h.event("session.created", {
    sessionID: "main",
    title: null,
    location: { directory: "/work/project" },
  });
  await sleep(150);

  const event = h.lastSnapshot().events.find((e) => e.session_id === "main");
  assert.equal(event.name, "Real name");
});

test("debug mode logs event types and payload keys", async (t) => {
  const logs = [];
  const instance = await createAgentLightV2({
    directory: "/work/project",
    getSession: async () => undefined,
    subscribe: () => (async function* () {})(),
    log: (level, message, extra) => logs.push({ level, message, extra }),
    fetch: async () => ({ ok: true, status: 200, text: async () => "" }),
    env: { AGENTLIGHT_DEBUG: "1" },
    timing: FAST_TIMING,
  });
  t.after(() => instance.dispose());

  await instance.handleEvent({ type: "permission.asked", data: { sessionID: "s1", id: "p1" } });

  const entry = logs.find((line) => line.message === "event permission.asked");
  assert.ok(entry, "expected a debug log line for the event");
  assert.deepEqual(entry.extra.payload_keys, ["sessionID", "id"]);
});

// Autostart tests drive the exported factory directly so `spawn`, `fetch`, and
// `timing` can be injected.
test("a healthy hub is not spawned", async (t) => {
  t.after(setEnv("AGENTLIGHT_AUTOSTART_BIN", "/opt/agentlight/agentlight-server"));

  const calls = [];
  let healthCount = 0;
  const fetchStub = async (url, options = {}) => {
    calls.push({ url: String(url), method: options.method });
    if (String(url).endsWith("/healthz")) {
      healthCount += 1;
      return { ok: true, status: 200, text: async () => "" };
    }
    return { ok: true, status: 200, text: async () => "" };
  };
  const spawn = spawnSpy();
  const client = trackingClient();

  const instance = await createAgentLightV2({
    directory: "/work/project",
    getSession: async () => undefined,
    subscribe: () => (async function* () {})(),
    log: (level, message, extra) => client.log(level, message, extra),
    spawn,
    fetch: fetchStub,
    timing: FAST_TIMING,
  });
  t.after(() => instance.dispose());

  await waitFor(() => healthCount >= 1);
  await sleep(20);

  assert.equal(spawn.calls.length, 0, "must not spawn when /healthz is healthy");
});

test("no autostart bin means no probe and no spawn", async (t) => {
  t.after(setEnv("AGENTLIGHT_AUTOSTART_BIN", undefined));

  const calls = [];
  const fetchStub = async (url, options = {}) => {
    calls.push({ url: String(url), method: options.method });
    return { ok: true, status: 200, text: async () => "" };
  };
  const spawn = spawnSpy();
  const client = trackingClient();

  const instance = await createAgentLightV2({
    directory: "/work/project",
    getSession: async () => undefined,
    subscribe: () => (async function* () {})(),
    log: (level, message, extra) => client.log(level, message, extra),
    spawn,
    fetch: fetchStub,
    timing: FAST_TIMING,
  });
  t.after(() => instance.dispose());
  await sleep(20);

  assert.equal(spawn.calls.length, 0, "must not spawn without AGENTLIGHT_AUTOSTART_BIN");
  assert.equal(
    calls.filter((call) => call.url.endsWith("/healthz")).length,
    0,
    "must be a no-op without AGENTLIGHT_AUTOSTART_BIN",
  );
});

test("an unreachable hub is spawned detached and unref'd, then awaited", async (t) => {
  t.after(setEnv("AGENTLIGHT_AUTOSTART_BIN", "/opt/agentlight/agentlight-server"));

  let healthCount = 0;
  const fetchStub = async (url) => {
    if (String(url).endsWith("/healthz")) {
      healthCount += 1;
      const healthy = healthCount > 1;
      return { ok: healthy, status: healthy ? 200 : 503, text: async () => "" };
    }
    return { ok: true, status: 200, text: async () => "" };
  };
  const spawn = spawnSpy();
  const client = trackingClient();

  const instance = await createAgentLightV2({
    directory: "/work/project",
    getSession: async () => undefined,
    subscribe: () => (async function* () {})(),
    log: (level, message, extra) => client.log(level, message, extra),
    spawn,
    fetch: fetchStub,
    timing: FAST_TIMING,
  });
  t.after(() => instance.dispose());

  await waitFor(() => spawn.unrefCount() >= 1);
  await waitFor(() => healthCount >= 2);
  await sleep(20);

  assert.equal(spawn.calls.length, 1, "expected exactly one spawn");
  assert.equal(spawn.calls[0].bin, "/opt/agentlight/agentlight-server");
  assert.deepEqual(spawn.calls[0].args, []);
  assert.equal(spawn.calls[0].options.detached, true);
  assert.equal(spawn.calls[0].options.stdio, "ignore");
  assert.equal(spawn.unrefCount(), 1, "the detached child must be unref'd");
  assert.equal(
    client.logs.filter((entry) => entry.level === "warn").length,
    0,
    "a hub that comes up must not warn",
  );
});

test("a hub that never becomes healthy is warned about and gives up", async (t) => {
  t.after(setEnv("AGENTLIGHT_AUTOSTART_BIN", "/opt/agentlight/agentlight-server"));

  const fetchStub = async (url) => {
    if (String(url).endsWith("/healthz")) {
      return { ok: false, status: 503, text: async () => "" };
    }
    return { ok: true, status: 200, text: async () => "" };
  };
  const spawn = spawnSpy();
  const client = trackingClient();

  const startedAt = Date.now();
  const instance = await createAgentLightV2({
    directory: "/work/project",
    getSession: async () => undefined,
    subscribe: () => (async function* () {})(),
    log: (level, message, extra) => client.log(level, message, extra),
    spawn,
    fetch: fetchStub,
    timing: FAST_TIMING,
  });
  t.after(() => instance.dispose());

  await waitFor(() => client.logs.some((entry) => /did not become healthy/.test(entry.message)));
  const elapsed = Date.now() - startedAt;

  assert.equal(spawn.calls.length, 1, "the binary should still be spawned");
  assert.equal(spawn.unrefCount(), 1);
  assert.ok(elapsed < 1000, `autostart should give up quickly, took ${elapsed}ms`);
  assert.equal(
    client.logs.filter((entry) => entry.level === "warn").length,
    1,
    "expected exactly one warning",
  );
});
