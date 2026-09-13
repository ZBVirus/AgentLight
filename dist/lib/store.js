"use strict";

import { invoke } from "./ipc.js";
import { MINI_SIZES, SIZES } from "./format.js";
import { renderLight, applyMiniTheme } from "../views/light.js";
import { renderDetail } from "../views/detail.js";

const $ = (id) => document.getElementById(id);

export let snapshot = null;
export let config = null;
export let view = "mini";

export function setSnapshot(next) {
  snapshot = next;
}

export function setConfig(next) {
  config = next;
}

export function collapseStyle() {
  return (config && config.collapse_style) || "single";
}

export function applyCollapseStyle() {
  document.body.dataset.collapse = collapseStyle();
  applyMiniTheme(config);
  if (view === "mini") setView("mini");
}

export function setView(next) {
  view = next;
  document.body.dataset.view = next;
  $("view-mini").classList.toggle("hidden", next !== "mini");
  $("view-detail").classList.toggle("hidden", next !== "detail");
  $("view-settings").classList.toggle("hidden", next !== "settings");
  const size =
    next === "mini"
      ? MINI_SIZES[collapseStyle()] || MINI_SIZES.single
      : SIZES[next] || MINI_SIZES.single;
  if (invoke) {
    invoke("resize_window", {
      width: size.width,
      height: size.height,
      mode: next,
    }).catch(() => {});
  }
}

export function renderPin() {
  $("btn-pin").classList.toggle("active", !!(config && config.always_on_top));
}

export function render() {
  renderLight(snapshot);
  renderDetail(snapshot);
  renderPin();
}

export async function refreshSnapshot() {
  if (!invoke) return;
  try {
    snapshot = await invoke("get_snapshot");
  } catch (error) {
    snapshot = { ok: false, exists: false, error: String(error), sessions: [], counts: {} };
  }
  render();
}

export async function loadConfig() {
  if (!invoke) return;
  try {
    config = await invoke("get_config");
  } catch (error) {
    config = null;
  }
  renderPin();
  applyCollapseStyle();
}
