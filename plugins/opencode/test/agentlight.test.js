// Tests for the opencode plugin's opencode-event -> AgentLight-status mapping.
//
// These drive `AgentLightPlugin` with a stub client and a stubbed `fetch`, so
// the mapping can be verified without a live opencode build. Run with:
//   node --test plugins/opencode/test/

import test from "node:test";
import assert from "node:assert/strict";

import { AgentLightPlugin } from "../agentlight.js";

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

// Fetch stub that answers /healthz (with a call-count controllable verdict) and
// accepts ingest posts. `healthyAfter` is the number of unhealthy probes before
// it starts reporting 200.
function healthFetch({ healthyAfter = 0 } = {}) {
  const calls = [];
  let healthCount = 0;
  const fetchStub = async (url, options = {}) => {
    calls.push({ url: String(url), method: options.method });
    if (String(url).endsWith("/healthz")) {
      healthCount += 1;
      const healthy = healthCount > healthyAfter;
      return { ok: healthy, status: healthy ? 200 : 503, text: async () => "" };
    }
    return { ok: true, status: 200, text: async () => "" };
  };
  fetchStub.calls = calls;
  fetchStub.healthCount = () => healthCount;
  return fetchStub;
}

function trackingClient() {
  const logs = [];
  return {
    logs,
    app: {
      log: async ({ body }) => {
        logs.push(body);
      },
    },
    session: { get: async () => ({ data: undefined }) },
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

// Save AGENTLIGHT_AUTOSTART_BIN and restore it when the returned function runs.
function setAutostartBin(value) {
  const previous = process.env.AGENTLIGHT_AUTOSTART_BIN;
  if (value === undefined) delete process.env.AGENTLIGHT_AUTOSTART_BIN;
  else process.env.AGENTLIGHT_AUTOSTART_BIN = value;
  return () => {
    if (previous === undefined) delete process.env.AGENTLIGHT_AUTOSTART_BIN;
    else process.env.AGENTLIGHT_AUTOSTART_BIN = previous;
  };
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

async function harness(sessionInfo = {}) {
  const calls = [];
  const previousFetch = globalThis.fetch;
  globalThis.fetch = async (url, options) => {
    const body = JSON.parse(options.body);
    const events = body.events || [];
    calls.push({ url, mode: body.mode, events, event: events[0] });
    return { ok: true, status: 200, text: async () => "" };
  };

  const client = {
    app: { log: async () => {} },
    session: { get: async ({ path }) => ({ data: sessionInfo[path.id] }) },
  };

  const hooks = await AgentLightPlugin({ directory: "/work/project", client });
  return {
    calls,
    emit: (type, properties) => hooks.event({ event: { type, properties } }),
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
      globalThis.fetch = previousFetch;
    },
  };
}

test("a main session going idle is inactive", async (t) => {
  const h = await harness();
  t.after(h.restore);

  await h.emit("session.created", {
    info: { id: "main", title: "Main", directory: "/work/project" },
  });
  await h.emit("session.status", { sessionID: "main", status: { type: "idle" } });
  await sleep(COALESCE_WAIT_MS);

  assert.equal(h.lastStatus(), "inactive");
});

test("a subagent going idle is done", async (t) => {
  const h = await harness();
  t.after(h.restore);

  await h.emit("session.created", {
    info: { id: "sub", parentID: "main", title: "Sub", directory: "/work/project" },
  });
  await h.emit("session.status", { sessionID: "sub", status: { type: "idle" } });
  await sleep(COALESCE_WAIT_MS);

  assert.equal(h.lastStatus(), "done");
});

test("an idle session with no prior created event resolves its parent via session.get", async (t) => {
  const h = await harness({ sub: { id: "sub", parentID: "main", title: "Sub" } });
  t.after(h.restore);

  await h.emit("session.idle", { sessionID: "sub" });
  await sleep(COALESCE_WAIT_MS);

  assert.equal(h.lastStatus(), "done");
});

test("idle while a permission is pending stays needs_help", async (t) => {
  const h = await harness();
  t.after(h.restore);

  await h.emit("session.created", {
    info: { id: "sub", parentID: "main", title: "Sub", directory: "/work/project" },
  });
  await h.emit("permission.asked", { sessionID: "sub", permissionID: "p1" });
  await h.emit("session.idle", { sessionID: "sub" });
  await sleep(COALESCE_WAIT_MS);

  assert.equal(h.lastStatus(), "needs_help");
});

test("permission.replied returns the session to active", async (t) => {
  const h = await harness();
  t.after(h.restore);

  await h.emit("permission.asked", { sessionID: "main", permissionID: "p1" });
  await h.emit("permission.replied", { sessionID: "main", permissionID: "p1" });
  await sleep(COALESCE_WAIT_MS);

  assert.equal(h.lastStatus(), "active");
});

test("a deleted session is done", async (t) => {
  const h = await harness();
  t.after(h.restore);

  await h.emit("session.deleted", {
    info: { id: "main", title: "Main", directory: "/work/project" },
  });
  await sleep(COALESCE_WAIT_MS);

  assert.equal(h.lastStatus(), "done");
});

test("a busy session is active", async (t) => {
  const h = await harness();
  t.after(h.restore);

  await h.emit("session.status", { sessionID: "main", status: { type: "busy" } });
  await sleep(COALESCE_WAIT_MS);

  assert.equal(h.lastStatus(), "active");
});

test("a session URL template decorates reported events and snapshots", async (t) => {
  t.after(setEnv("AGENTLIGHT_SESSION_URL_TEMPLATE", "http://x/session/{id}"));
  const h = await harness();
  t.after(h.restore);

  await h.emit("session.created", {
    info: { id: "main", title: "Main", directory: "/work/project" },
  });
  await h.emit("session.status", { sessionID: "main", status: { type: "busy" } });
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

test("without a session URL template, url is null", async (t) => {
  t.after(setEnv("AGENTLIGHT_SESSION_URL_TEMPLATE", undefined));
  const h = await harness();
  t.after(h.restore);

  await h.emit("session.status", { sessionID: "main", status: { type: "busy" } });
  await sleep(COALESCE_WAIT_MS);

  const reported = h.calls.find(
    (call) => call.event && call.event.session_id === "main" && call.event.status === "active",
  );
  assert.ok(reported, "expected a reported active event");
  assert.equal(reported.event.url, null);

  const snapshot = h.lastSnapshot();
  assert.ok(snapshot, "expected a startup snapshot");
  const snapEvent = snapshot.events.find((event) => event.session_id === "main");
  assert.ok(snapEvent, "snapshot should include the session");
  assert.equal(snapEvent.url, null);
});

test("a startup snapshot is sent with mode snapshot and includes a created session", async (t) => {
  const h = await harness();
  t.after(h.restore);

  await h.emit("session.created", {
    info: { id: "main", title: "Main", directory: "/work/project" },
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

  await h.emit("session.created", {
    info: { id: "live", title: "Live", directory: "/work/project" },
  });
  await h.emit("session.status", { sessionID: "live", status: { type: "busy" } });

  for (let i = 0; i < 7; i++) {
    await h.emit("session.deleted", {
      info: { id: `done-${i}`, title: `Done ${i}`, directory: "/work/project" },
    });
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

  await sleep(200);

  assert.ok(
    h.snapshots().length > 1,
    `expected more than one snapshot, got ${h.snapshots().length}`,
  );
});

test("a healthy hub is not spawned", async (t) => {
  t.after(setAutostartBin("/opt/agentlight/agentlight-server"));

  const fetchStub = healthFetch({ healthyAfter: 0 });
  const spawn = spawnSpy();
  const client = trackingClient();

  await AgentLightPlugin(
    { directory: "/work/project", client },
    { spawn, fetch: fetchStub, timing: FAST_TIMING },
  );

  await waitFor(() => fetchStub.healthCount() >= 1);
  await sleep(20);

  assert.equal(spawn.calls.length, 0, "must not spawn when /healthz is healthy");
});

test("no autostart bin means no probe and no spawn", async (t) => {
  t.after(setAutostartBin(undefined));

  const fetchStub = healthFetch({ healthyAfter: 0 });
  const spawn = spawnSpy();
  const client = trackingClient();

  await AgentLightPlugin(
    { directory: "/work/project", client },
    { spawn, fetch: fetchStub, timing: FAST_TIMING },
  );
  await sleep(20);

  assert.equal(spawn.calls.length, 0, "must not spawn without AGENTLIGHT_AUTOSTART_BIN");
  assert.equal(
    fetchStub.calls.filter((call) => call.url.endsWith("/healthz")).length,
    0,
    "must be a no-op without AGENTLIGHT_AUTOSTART_BIN",
  );
});

test("an unreachable hub is spawned detached and unref'd, then awaited", async (t) => {
  t.after(setAutostartBin("/opt/agentlight/agentlight-server"));

  const fetchStub = healthFetch({ healthyAfter: 1 });
  const spawn = spawnSpy();
  const client = trackingClient();

  await AgentLightPlugin(
    { directory: "/work/project", client },
    { spawn, fetch: fetchStub, timing: FAST_TIMING },
  );

  await waitFor(() => spawn.unrefCount() >= 1);
  await waitFor(() => fetchStub.healthCount() >= 2);
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
  t.after(setAutostartBin("/opt/agentlight/agentlight-server"));

  const fetchStub = healthFetch({ healthyAfter: Number.POSITIVE_INFINITY });
  const spawn = spawnSpy();
  const client = trackingClient();

  const startedAt = Date.now();
  await AgentLightPlugin(
    { directory: "/work/project", client },
    { spawn, fetch: fetchStub, timing: FAST_TIMING },
  );

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

