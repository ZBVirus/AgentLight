"use strict";

import { invoke } from "../lib/ipc.js";
import { relativeTime } from "../lib/format.js";
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

const DEFAULT_SERVER_BIND = "127.0.0.1:8787";

// Whether the persisted config carries an admin token hash. Lets the token
// hint explain that a secret is set without ever putting the hash in the input.
let adminTokenSet = false;

export function renderServerUrl() {
  const bind = $("set-server-bind").value.trim();
  const token = $("set-server-token").value.trim();
  const base = bind ? `http://${bind}/` : "";
  $("set-server-url").value = token
    ? `${base}?token=${encodeURIComponent(token)}`
    : base;
  $("set-server-url-hint").textContent = token
    ? "Includes the token typed above, for this session only. Once saved, the plaintext cannot be recovered; pair other devices with the code below."
    : "No token in the field, so devices should pair with the code below. Use 0.0.0.0:8787 to accept LAN connections.";
  renderTokenHint();
}

function renderTokenHint() {
  const typed = $("set-server-token").value.trim();
  $("set-server-token-hint").textContent = typed
    ? "A new secret is in the field. Start or Save hashes it before storing; it cannot be shown again."
    : adminTokenSet
      ? "A token is set. Type a new one to replace it, or Clear to remove it."
      : "No admin token. The API is open on loopback, or available to paired devices.";
}

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
  $("set-server-bind").value = config.server_bind || DEFAULT_SERVER_BIND;
  // The admin token is write-only: never render the stored hash back here.
  $("set-server-token").value = "";
  // Assigning `oninput` (instead of addEventListener) keeps re-population from
  // stacking listeners.
  $("set-server-bind").oninput = renderServerUrl;
  $("set-server-token").oninput = renderServerUrl;
  $("btn-new-code").onclick = regeneratePairing;
  $("btn-server-toggle").onclick = toggleServer;
  $("btn-server-token-clear").onclick = clearAdminToken;
  renderServerUrl();
  refreshServerStatus();
}

export async function refreshServerStatus() {
  if (!invoke) return;
  try {
    renderServerStatus(await invoke("get_server_status"));
  } catch (error) {
    renderServerStatus(null);
  }
}

function renderServerStatus(status) {
  const running = !!(status && status.enabled);
  adminTokenSet = !!(status && status.admin_token_set);
  $("set-server-status").textContent =
    running && status.url ? `Running at ${status.url.replace(/\/$/, "")}` : "Stopped";
  $("btn-server-toggle").textContent = running ? "Stop server" : "Start server";
  $("set-pair-code").textContent = running ? status.pairing_code || "--------" : "—";
  $("btn-new-code").disabled = !running;
  $("set-pair-hint").textContent = running
    ? status.pairing_expires_at
      ? `Expires ${new Date(status.pairing_expires_at).toLocaleTimeString()}`
      : ""
    : "Start the server to pair a device.";
  renderDevices(running ? status.devices || [] : []);
  renderServerUrl();
}

function renderDevices(devices) {
  const list = $("set-devices");
  list.innerHTML = "";
  for (const device of devices) {
    list.appendChild(deviceRow(device));
  }
  $("set-devices-empty").classList.toggle("hidden", devices.length > 0);
}

function deviceRow(device) {
  const li = document.createElement("li");
  li.className = "device";

  const meta = document.createElement("div");
  meta.className = "device-meta";
  const name = document.createElement("span");
  name.className = "device-name";
  name.textContent = device.name || device.id;
  const seen = document.createElement("span");
  seen.className = "device-sub";
  seen.textContent = `last seen ${relativeTime(device.last_seen) || "never"}`;
  meta.append(name, seen);

  const revoke = document.createElement("button");
  revoke.type = "button";
  revoke.className = "ghost";
  revoke.textContent = "Revoke";
  revoke.addEventListener("click", () => revokeDevice(device.id));

  li.append(meta, revoke);
  return li;
}

async function regeneratePairing() {
  if (!invoke) return;
  try {
    await invoke("regenerate_pairing");
    toast("New pairing code");
  } catch (error) {
    toast(`Could not make a code: ${error}`);
  }
  await refreshServerStatus();
}

async function revokeDevice(id) {
  if (!invoke) return;
  try {
    await invoke("revoke_device", { id });
    toast("Device revoked");
  } catch (error) {
    toast(`Revoke failed: ${error}`);
  }
  await refreshServerStatus();
}

// Overlay the server form fields onto `next`, preserving the loaded config
// (including any saved token hash) when the token input is left blank.
function applyServerFields(next) {
  next.server_bind = $("set-server-bind").value.trim() || DEFAULT_SERVER_BIND;
  const typed = $("set-server-token").value.trim();
  next.server_token = typed || null;
  if (typed) {
    // The shell hashes the plaintext; drop the loaded hash so it cannot win.
    next.server_token_hash = null;
  }
  return next;
}

function clearTokenInput() {
  $("set-server-token").value = "";
  renderServerUrl();
}

async function toggleServer() {
  if (!invoke || !config) return;
  const running = $("btn-server-toggle").textContent === "Stop server";
  const next = applyServerFields({ ...config });
  next.server_enabled = !running;
  try {
    setConfig(await invoke("set_config", { config: next }));
    clearTokenInput();
    toast(next.server_enabled ? "Server started" : "Server stopped");
  } catch (error) {
    toast(`Server ${running ? "stop" : "start"} failed: ${error}`);
  }
  await refreshServerStatus();
}

async function clearAdminToken() {
  if (!invoke || !config) return;
  const next = applyServerFields({ ...config });
  next.server_token = null;
  next.server_token_hash = null;
  try {
    setConfig(await invoke("set_config", { config: next }));
    clearTokenInput();
    toast("Admin token cleared");
  } catch (error) {
    toast(`Clear failed: ${error}`);
  }
  await refreshServerStatus();
}

export async function saveSettings(event) {
  if (event) event.preventDefault();
  if (!invoke || !config) return;
  const next = applyServerFields({
    ...config,
    state_path: $("set-state-path").value.trim() || null,
    always_on_top: $("set-top").checked,
    start_at_login: $("set-autostart").checked,
    notifications: $("set-notifications").checked,
    show_done: $("set-show-done").checked,
    yellow_mode: $("set-yellow-mode").value,
    collapse_style: $("set-collapse-style").value,
    poll_ms: Number($("set-poll").value) || 1500,
  });
  try {
    setConfig(await invoke("set_config", { config: next }));
    applyCollapseStyle();
    clearTokenInput();
    toast("Saved");
    await refreshSnapshot();
    await refreshServerStatus();
    setView("detail");
  } catch (error) {
    toast(`Save failed: ${error}`);
  }
}
