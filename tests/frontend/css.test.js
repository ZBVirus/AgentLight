// Static checks on dist/style.css for the collapsed-view sizes. These are the
// values the app shipped before the vmin experiment and must stay fixed.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const css = readFileSync(path.resolve(here, "..", "..", "dist", "style.css"), "utf8");

function rule(selector) {
  // Grab the first block whose selector list contains `selector`.
  const re = new RegExp("(^|[,}])\\s*" + selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&") + "\\s*\\{([^}]*)\\}", "m");
  const match = css.match(re);
  return match ? match[2] : "";
}

test("collapsed single light keeps its fixed size", () => {
  const base = rule(".light");
  assert.match(base, /width:\s*62px/);
  assert.match(base, /height:\s*62px/);
  const core = rule(".light-core");
  assert.match(core, /width:\s*40px/);
  assert.match(core, /height:\s*40px/);
});

test("collapsed triple lights keep their fixed size", () => {
  const light = rule(".mini-light");
  assert.match(light, /width:\s*42px/);
  assert.match(light, /height:\s*42px/);
  const core = rule(".mini-light .core");
  assert.match(core, /width:\s*24px/);
  assert.match(core, /height:\s*24px/);
});

test("collapsed labels keep their fixed size", () => {
  assert.match(rule(".mini-label"), /font-size:\s*10px/);
});

test("horizontal triple lights run green to red", () => {
  assert.match(rule('body[data-collapse="triple"] .mini-triple'), /row-reverse/);
});

test("the vmin scaling and the mini light override are gone", () => {
  assert.doesNotMatch(css, /vmin/);
  assert.doesNotMatch(css, /\.view\.mini \.light\s*\{/);
});
