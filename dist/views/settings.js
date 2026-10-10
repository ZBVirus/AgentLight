"use strict";

import { invoke, listen } from "../lib/ipc.js";
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
const DEFAULT_HUB_URL = "http://127.0.0.1:8787";

// Built-in palette, matching the CSS custom properties. A color input seeded
// with one of these is treated as "not customized" on save so the config keeps
// its null and the palette stays future-proof.
const MINI_BUILTIN = {
  red: "#e06c75",
  orange: "#e5c07b",
  green: "#98c379",
};

const MINI_COLORS = ["red", "orange", "green"];
const HEX_COLOR = /^#[0-9a-f]{6}$/i;

// Whether the persisted config carries an admin token hash. Lets the token
// hint explain that a secret is set without ever putting the hash in the input.
let adminTokenSet = false;

// The backend emits this whenever the live server starts or stops, including
// from a path other than the Settings buttons. Keep the button label and
// status in sync with the real handle.
if (listen) {
  listen("server-changed", () => {
    refreshServerStatus();
  });
}

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

// Show the fields that belong to the selected source: the file path in file
// mode, the hub URL/token in hub mode.
function renderSourceFields() {
  const hub = $("set-source-kind").value === "hub";
  $("set-file-fields").classList.toggle("hidden", hub);
  $("set-hub-fields").classList.toggle("hidden", !hub);
}

export function populateSettings() {
  if (!config) return;
  $("set-source-kind").value = config.source_kind || "file";
  $("set-hub-url").value = config.hub_url || DEFAULT_HUB_URL;
  // The hub token is a client credential and must be stored plaintext, so it is
  // the one secret we can render back; clearing the field clears it on save.
  $("set-hub-token").value = config.hub_token || "";
  $("set-state-path").value = config.state_path || (snapshot ? snapshot.state_path : "");
  $("set-top").checked = !!config.always_on_top;
  $("set-autostart").checked = !!config.start_at_login;
  $("set-notifications").checked = !!config.notifications;
  setTriggers("notification", config.notification_trigger);
  $("set-show-done").checked = !!config.show_done;
  $("set-alarms-enabled").checked = !!config.alarms_enabled;
  setTriggers("alarm", config.alarm_trigger);
  $("set-alarm-sound").value = config.alarm_sound || "";
  $("set-alarm-volume").value = config.alarm_volume ?? 70;
  $("set-alarm-volume").oninput = renderAlarmVolume;
  renderAlarmVolume();
  $("set-yellow-mode").value = config.yellow_mode || "any_inactive";
  $("set-collapse-style").value = config.collapse_style || "single";
  // A color input only understands #rrggbb, so seed the built-in hex when the
  // config carries nothing (or a color the picker cannot represent).
  for (const key of MINI_COLORS) {
    const value = config[`mini_${key}`];
    $(`set-mini-${key}`).value = HEX_COLOR.test(value || "")
      ? value
      : MINI_BUILTIN[key];
  }
  $("set-mini-show-labels").checked = config.mini_show_labels !== false;
  $("set-poll").value = config.poll_ms || 1500;
  $("set-session-link").value = config.session_link || "opencode_v2";
  $("set-session-link-base").value = config.session_link_base || "";
  $("set-server-bind").value = config.server_bind || DEFAULT_SERVER_BIND;
  // The admin token is write-only: never render the stored hash back here.
  $("set-server-token").value = "";
  // Assigning `oninput` (instead of addEventListener) keeps re-population from
  // stacking listeners.
  $("set-source-kind").onchange = renderSourceFields;
  $("set-server-bind").oninput = renderServerUrl;
  $("set-server-token").oninput = renderServerUrl;
  $("set-notifications").onchange = renderNotificationRows;
  $("set-alarms-enabled").onchange = renderAlarmRows;
  $("set-session-link").onchange = renderSessionLinkFields;
  // "Any status change" acts as a select-all within its trigger row.
  $("set-notification-any").onchange = () => syncAnyTrigger("notification");
  $("set-alarm-any").onchange = () => syncAnyTrigger("alarm");
  // Apply "Always on top" immediately so the toolbar pin and this checkbox
  // stay in sync without a Save.
  $("set-top").onchange = applyAlwaysOnTop;
  $("btn-new-code").onclick = regeneratePairing;
  $("btn-server-toggle").onclick = toggleServer;
  $("btn-server-token-clear").onclick = clearAdminToken;
  $("btn-alarm-sound-pick").onclick = chooseAlarmSound;
  $("btn-alarm-sound-clear").onclick = clearAlarmSound;
  $("btn-reset-colors").onclick = resetColors;
  renderSourceFields();
  renderNotificationRows();
  renderAlarmRows();
  renderSessionLinkFields();
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

// Apply the "Always on top" checkbox right away; the shell persists it and
// emits `config-changed`, which refreshes the toolbar pin.
async function applyAlwaysOnTop() {
  if (!invoke) return;
  try {
    await invoke("set_always_on_top", { enabled: $("set-top").checked });
  } catch (error) {
    toast(`Pin failed: ${error}`);
  }
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

// Show and enable the notification trigger only while notifications are on.
function renderNotificationRows() {
  const rows = $("set-notification-rows");
  if (!rows) return;
  const enabled = $("set-notifications").checked;
  rows.classList.toggle("hidden", !enabled);
  for (const el of rows.querySelectorAll("input, select, button")) {
    el.disabled = !enabled;
  }
}

// Show the server base URL field only for the modes that use it.
function renderSessionLinkFields() {
  const mode = $("set-session-link").value;
  const usesBase = mode === "opencode_v1" || mode === "opencode_v2";
  $("set-session-link-base-field").classList.toggle("hidden", !usesBase);
  $("set-session-link-base").placeholder =
    mode === "opencode_v1" ? "http://localhost:4096" : "http://127.0.0.1:49374";
}

// Show and enable the alarm trigger/sound rows only while alarms are on.
function renderAlarmRows() {
  const rows = $("set-alarm-rows");
  if (!rows) return;
  const enabled = $("set-alarms-enabled").checked;
  rows.classList.toggle("hidden", !enabled);
  for (const el of rows.querySelectorAll("input, select, button")) {
    el.disabled = !enabled;
  }
}

// Seed the color inputs with the built-in palette. Saving afterwards treats
// them as uncustomized and persists nulls, restoring the defaults.
function resetColors() {
  for (const key of MINI_COLORS) {
    $(`set-mini-${key}`).value = MINI_BUILTIN[key];
  }
}

// A color input always holds a value, so "customized" means it differs from the
// built-in hex. Returning null keeps the built-in palette for that light.
function miniColorValue(key) {
  const value = $(`set-mini-${key}`).value.trim();
  return value && value.toLowerCase() !== MINI_BUILTIN[key] ? value : null;
}

async function chooseAlarmSound() {
  if (!invoke) return;
  try {
    const path = await invoke("pick_sound_file");
    if (path) $("set-alarm-sound").value = path;
  } catch (error) {
    toast(`Could not pick sound: ${error}`);
  }
}

function clearAlarmSound() {
  $("set-alarm-sound").value = "";
}

// Show the current alarm volume beside the slider.
function renderAlarmVolume() {
  $("set-alarm-volume-value").textContent = `${$("set-alarm-volume").value}%`;
}

// Trigger fields (`notification_trigger` / `alarm_trigger`) are lists of
// `needs_help` / `done` / `any_status`. A single string is accepted too.
const TRIGGER_IDS = {
  notification: {
    needsHelp: "set-notification-needs-help",
    done: "set-notification-done",
    any: "set-notification-any",
  },
  alarm: {
    needsHelp: "set-alarm-needs-help",
    done: "set-alarm-done",
    any: "set-alarm-any",
  },
};

// An explicit empty list means "no trigger selected"; a missing value falls
// back to the default.
function triggerValues(value) {
  if (Array.isArray(value)) return value;
  if (typeof value === "string" && value) return [value];
  return ["needs_help"];
}

// "Any status change" is a select-all: it already matches every transition, so
// when it is set the specific triggers are shown selected too.
function setTriggers(kind, value) {
  let selected = triggerValues(value);
  if (selected.includes("any_status")) {
    selected = ["needs_help", "done", "any_status"];
  }
  const ids = TRIGGER_IDS[kind];
  $(ids.needsHelp).checked = selected.includes("needs_help");
  $(ids.done).checked = selected.includes("done");
  $(ids.any).checked = selected.includes("any_status");
}

// Checking "Any status change" selects the specific triggers; clearing it
// clears them, so the row behaves as one "select all" control.
function syncAnyTrigger(kind) {
  const ids = TRIGGER_IDS[kind];
  const on = $(ids.any).checked;
  $(ids.needsHelp).checked = on;
  $(ids.done).checked = on;
}

function readTriggers(kind) {
  const ids = TRIGGER_IDS[kind];
  const selected = [];
  if ($(ids.needsHelp).checked) selected.push("needs_help");
  if ($(ids.done).checked) selected.push("done");
  if ($(ids.any).checked) selected.push("any_status");
  return selected;
}

export async function saveSettings(event) {
  if (event) event.preventDefault();
  if (!invoke || !config) return;
  const sourceKind = $("set-source-kind").value;
  const next = applyServerFields({
    ...config,
    source_kind: sourceKind,
    hub_url: $("set-hub-url").value.trim() || DEFAULT_HUB_URL,
    hub_token: $("set-hub-token").value.trim() || null,
    // The state-file input is seeded from the snapshot's resolved clawlight
    // path even in hub mode; never write that back for a non-file source, or
    // an unchanged save would restart the hub source.
    state_path:
      sourceKind === "file" ? $("set-state-path").value.trim() || null : config.state_path,
    always_on_top: $("set-top").checked,
    start_at_login: $("set-autostart").checked,
    notifications: $("set-notifications").checked,
    notification_trigger: readTriggers("notification"),
    show_done: $("set-show-done").checked,
    alarms_enabled: $("set-alarms-enabled").checked,
    alarm_trigger: readTriggers("alarm"),
    alarm_sound: $("set-alarm-sound").value.trim() || null,
    alarm_volume: Number($("set-alarm-volume").value),
    yellow_mode: $("set-yellow-mode").value,
    collapse_style: $("set-collapse-style").value,
    mini_red: miniColorValue("red"),
    mini_orange: miniColorValue("orange"),
    mini_green: miniColorValue("green"),
    mini_show_labels: $("set-mini-show-labels").checked,
    poll_ms: Number($("set-poll").value) || 1500,
    session_link: $("set-session-link").value,
    session_link_base: $("set-session-link-base").value.trim() || null,
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
