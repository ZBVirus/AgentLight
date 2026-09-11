"use strict";

import { invoke } from "../lib/ipc.js";
import { toast } from "../lib/toast.js";
import {
  applyCollapseStyle,
  config,
  refreshSnapshot,
  setConfig,
  setView,
  snapshot,
} from "../lib/store.js";

const $ = (id) => document.getElementById(id);

export function populateSettings() {
  if (!config) return;
  $("set-state-path").value = config.state_path || (snapshot ? snapshot.state_path : "");
  $("set-top").checked = !!config.always_on_top;
  $("set-autostart").checked = !!config.start_at_login;
  $("set-notifications").checked = !!config.notifications;
  $("set-show-done").checked = !!config.show_done;
  $("set-yellow-mode").value = config.yellow_mode || "any_inactive";
  $("set-collapse-style").value = config.collapse_style || "single";
  $("set-poll").value = config.poll_ms || 1500;
}

export async function saveSettings(event) {
  if (event) event.preventDefault();
  if (!invoke || !config) return;
  const next = {
    state_path: $("set-state-path").value.trim() || null,
    always_on_top: $("set-top").checked,
    start_at_login: $("set-autostart").checked,
    notifications: $("set-notifications").checked,
    show_done: $("set-show-done").checked,
    yellow_mode: $("set-yellow-mode").value,
    collapse_style: $("set-collapse-style").value,
    poll_ms: Number($("set-poll").value) || 1500,
  };
  try {
    setConfig(await invoke("set_config", { config: next }));
    applyCollapseStyle();
    toast("Saved");
    await refreshSnapshot();
    setView("detail");
  } catch (error) {
    toast(`Save failed: ${error}`);
  }
}
