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
    calls.push({ url, event: JSON.parse(options.body).events[0] });
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
      const call = calls.at(-1);
      return call && call.event.status;
    },
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
