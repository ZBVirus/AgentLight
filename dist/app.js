"use strict";

import { invoke, listen } from "./lib/ipc.js";
import {
  applyCollapseStyle,
  config,
  loadConfig,
  refreshSnapshot,
  render,
  renderPin,
  setConfig,
  setSnapshot,
  setView,
  snapshot,
  view,
} from "./lib/store.js";
import { clearDone, renderDetail } from "./views/detail.js";
import { populateSettings, saveSettings } from "./views/settings.js";
import { wireMini } from "./lib/drag.js";
import { toast } from "./lib/toast.js";

const $ = (id) => document.getElementById(id);

function wire() {
  wireMini();
  $("btn-collapse").addEventListener("click", () => setView("mini"));
  $("btn-settings").addEventListener("click", () => {
    populateSettings();
    setView("settings");
  });
  $("btn-settings-back").addEventListener("click", () => setView("detail"));
  $("btn-clear-done").addEventListener("click", clearDone);

  $("btn-pin").addEventListener("click", async () => {
    if (!invoke) return;
    const enabled = !(config && config.always_on_top);
    try {
      await invoke("set_always_on_top", { enabled });
      if (config) config.always_on_top = enabled;
      renderPin();
    } catch (error) {
      toast(`Pin failed: ${error}`);
    }
  });
  $("btn-min").addEventListener("click", () => invoke && invoke("window_minimize"));
  $("btn-hide").addEventListener("click", () => invoke && invoke("window_hide"));

  const browse = async () => {
    if (!invoke) return;
    try {
      const path = await invoke("pick_state_file");
      if (path) {
        // `pick_state_file` already saved the choice; reflect it in the text
        // box so a later Save does not write the stale path back.
        $("set-state-path").value = path;
        toast("State file updated");
        await loadConfig();
        await refreshSnapshot();
      }
    } catch (error) {
      toast(`Could not set file: ${error}`);
    }
  };
  $("btn-pick").addEventListener("click", browse);
  $("btn-browse").addEventListener("click", browse);

  $("settings-form").addEventListener("submit", saveSettings);

  if (listen) {
    listen("state-changed", (event) => {
      setSnapshot(event.payload);
      render();
    });
    listen("config-changed", (event) => {
      setConfig(event.payload);
      renderPin();
      applyCollapseStyle();
    });
    listen("open-settings", () => {
      populateSettings();
      setView("settings");
    });
  }
}

async function boot() {
  if (!invoke) {
    document.body.innerHTML =
      '<div style="padding:20px;color:#e06c75">Tauri API unavailable.</div>';
    return;
  }
  wire();
  await loadConfig();
  await refreshSnapshot();
  populateSettings();
  setView("mini");
  // Keep relative timestamps honest even without state changes.
  setInterval(() => {
    if (view === "detail") renderDetail(snapshot);
  }, 30000);
}

boot();
