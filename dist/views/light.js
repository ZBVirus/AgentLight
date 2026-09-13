"use strict";

import { AGG_LABEL } from "../lib/format.js";

const $ = (id) => document.getElementById(id);

export function setLights(color) {
  for (const el of document.querySelectorAll(".light")) {
    el.classList.remove("red", "orange", "green", "gray");
    el.classList.add(color);
  }
}

// Each collapsed light is independent: lit when at least one session has
// that status. No "any needs_help wins" aggregation here.
export function setMiniState(state) {
  $("mini-red").classList.toggle("on", !!(state && state.red));
  $("mini-orange").classList.toggle("on", !!(state && state.yellow));
  $("mini-green").classList.toggle("on", !!(state && state.green));
}

function chip(colorClass, label) {
  const el = document.createElement("span");
  el.className = `chip ${colorClass}`;
  const dot = document.createElement("span");
  dot.className = "dot";
  el.appendChild(dot);
  el.appendChild(document.createTextNode(label));
  return el;
}

export function renderLight(snapshot) {
  const label = $("agg-label");
  const chips = $("chips");
  const pick = $("btn-pick");
  chips.replaceChildren();

  if (!snapshot) {
    setLights("gray");
    setMiniState(null);
    label.textContent = "Starting…";
    pick.classList.add("hidden");
    return;
  }

  setLights(snapshot.aggregate || "gray");
  label.textContent = AGG_LABEL[snapshot.aggregate] || "No sessions";

  if (!snapshot.ok) {
    setMiniState(null);
    const hub = snapshot.source_kind === "hub";
    let fallback;
    if (hub) {
      fallback = snapshot.exists
        ? "Could not reach the hub"
        : "Waiting for the hub…";
    } else {
      fallback = snapshot.exists
        ? "Could not read state file"
        : "Waiting for clawlight…";
    }
    label.textContent = snapshot.error || fallback;
    pick.classList.remove("hidden");
    return;
  }
  pick.classList.add("hidden");

  const counts = snapshot.counts || {};
  setMiniState({
    red: counts.needs_help > 0,
    yellow: counts.inactive > 0,
    green: counts.active > 0,
  });
  const rows = [
    ["red", counts.needs_help, "needs help"],
    ["orange", counts.inactive, "paused"],
    ["green", counts.active, "working"],
    ["gray", counts.done, "done"],
  ];
  let any = false;
  for (const [color, value, text] of rows) {
    if (value > 0) {
      any = true;
      chips.appendChild(chip(color, `${value} ${text}`));
    }
  }
  if (!any) {
    chips.appendChild(chip("gray", "no live sessions"));
  }
}
