"use strict";

import { AGG_LABEL } from "../lib/format.js";

const $ = (id) => document.getElementById(id);

// Mirrors the config field names to the CSS custom properties consumed by the
// light and chip rules. Setting them on the document root makes the user's
// palette apply to both the collapsed and expanded (detail) views.
const MINI_COLOR_VARS = [
  ["mini_red", "--mini-red"],
  ["mini_orange", "--mini-orange"],
  ["mini_green", "--mini-green"],
];

// Apply the user's color customization: the three light colors become custom
// properties (falling back to the built-in palette in CSS) and the no-labels
// class hides the captions beside the collapsed lights.
export function applyMiniTheme(config) {
  const root = document.documentElement;
  for (const [field, variable] of MINI_COLOR_VARS) {
    const value = config && config[field];
    if (value) root.style.setProperty(variable, value);
    else root.style.removeProperty(variable);
  }
  const mini = $("view-mini");
  if (mini) {
    mini.classList.toggle(
      "no-labels",
      !!(config && config.mini_show_labels === false),
    );
  }
}

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
  const miniLabel = $("mini-label");
  const setLabel = (text) => {
    label.textContent = text;
    if (miniLabel) miniLabel.textContent = text;
  };
  const chips = $("chips");
  const pick = $("btn-pick");
  chips.replaceChildren();

  if (!snapshot) {
    setLights("gray");
    setMiniState(null);
    setLabel("Starting…");
    pick.classList.add("hidden");
    return;
  }

  setLights(snapshot.aggregate || "gray");
  setLabel(AGG_LABEL[snapshot.aggregate] || "No sessions");

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
    setLabel(snapshot.error || fallback);
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
