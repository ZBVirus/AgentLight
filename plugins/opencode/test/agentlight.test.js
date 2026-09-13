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
