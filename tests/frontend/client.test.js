// Tests the hub's built-in web client (client.html) pairing flow in jsdom, so
// the "open server skips pairing" behavior runs on Linux without a browser.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { JSDOM } from "jsdom";

const here = path.dirname(fileURLToPath(import.meta.url));
const html = readFileSync(
  path.resolve(here, "..", "..", "crates", "agentlight-server", "src", "client.html"),
  "utf8",
);

function load({ auth = "bearer", url = "http://hub.local/", snapshotStatus = 200 } = {}) {
  const calls = [];
  const dom = new JSDOM(html, {
    runScripts: "dangerously",
    url,
    beforeParse(window) {
      window.fetch = async (input) => {
        const target = String(input);
        calls.push(target);
        if (target.indexOf("/healthz") === 0) {
          return { ok: true, status: 200, json: async () => ({ status: "ok", capabilities: { auth } }) };
        }
        if (target.indexOf("/api/v1/snapshot") === 0) {
          if (snapshotStatus !== 200) {
            return { ok: false, status: snapshotStatus, json: async () => ({}), text: async () => "" };
          }
          return {
            ok: true,
            status: 200,
            json: async () => ({
              ok: true,
              aggregate: "gray",
              counts: { total: 0 },
              sessions: [],
            }),
          };
        }
        return { ok: true, status: 200, json: async () => ({}) };
      };
      window.EventSource = class {
        addEventListener() {}
        close() {}
      };
    },
  });
  return { dom, calls };
}

const tick = () => new Promise((resolve) => setTimeout(resolve, 25));

test("an open server skips the pairing form", async () => {
  const { dom } = load({ auth: "none" });
  await tick();
  const doc = dom.window.document;
  assert.equal(doc.getElementById("state").hidden, false);
  assert.equal(doc.getElementById("pair-panel").hidden, true);
});

test("a server that requires auth shows the pairing form", async () => {
  const { dom } = load({ auth: "bearer" });
  await tick();
  const doc = dom.window.document;
  assert.equal(doc.getElementById("pair-panel").hidden, false);
  assert.equal(doc.getElementById("state").hidden, true);
});

test("a token in the URL skips the healthz probe", async () => {
  const { dom, calls } = load({ auth: "bearer", url: "http://hub.local/?token=abc" });
  await tick();
  assert.equal(calls.some((c) => c.indexOf("/healthz") === 0), false);
  assert.equal(dom.window.document.getElementById("state").hidden, false);
});

test("a rejected saved token returns to pairing with a message", async () => {
  const { dom } = load({ url: "http://hub.local/?token=stale", snapshotStatus: 401 });
  await tick();
  const doc = dom.window.document;
  assert.equal(doc.getElementById("pair-panel").hidden, false);
  assert.match(doc.getElementById("pair-error").textContent, /no longer valid/);
});
