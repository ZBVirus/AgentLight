"use strict";

// With `withGlobalTauri: true`, Tauri exposes its JS API on window.__TAURI__.
// Everything else (file IO, parsing, writes) happens in Rust commands.
const tauri = window.__TAURI__ || {};
const invoke = tauri.core ? tauri.core.invoke : null;
const listen = tauri.event ? tauri.event.listen : null;
const getCurrentWindow = tauri.window ? tauri.window.getCurrentWindow : null;

const MINI_SIZES = {
  single: { width: 88, height: 88 },
  triple: { width: 172, height: 68 },
};

const SIZES = {
  detail: { width: 420, height: 548 },
  settings: { width: 420, height: 600 },
};

const AGG_LABEL = {
  red: "Needs input",
  orange: "Idle",
  green: "All running",
  gray: "No sessions",
};

const STATUS_CLASS = {
  needs_help: "red",
  active: "green",
  inactive: "orange",
  done: "gray",
};

let snapshot = null;
let config = null;
let view = "mini";
let toastTimer = null;

const $ = (id) => document.getElementById(id);

function collapseStyle() {
  return (config && config.collapse_style) || "single";
}

function applyCollapseStyle() {
  document.body.dataset.collapse = collapseStyle();
  if (view === "mini") setView("mini");
}

function setView(next) {
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
    invoke("resize_window", size).catch(() => {});
  }
}

function toast(message) {
  const el = $("toast");
  el.textContent = message;
  el.classList.remove("hidden");
  el.classList.add("show");
  if (toastTimer) clearTimeout(toastTimer);
  toastTimer = setTimeout(() => {
    el.classList.remove("show");
    setTimeout(() => el.classList.add("hidden"), 220);
  }, 1800);
}

function relativeTime(iso) {
  if (!iso) return "";
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return "";
  const seconds = Math.max(0, Math.round((Date.now() - then) / 1000));
  if (seconds < 45) return "just now";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 48) return `${hours}h ago`;
  const days = Math.round(hours / 24);
  return `${days}d ago`;
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

function setLights(color) {
  for (const el of document.querySelectorAll(".light")) {
    el.classList.remove("red", "orange", "green", "gray");
    el.classList.add(color);
  }
}

// Each collapsed light is independent: lit when at least one session has
// that status. No "any needs_help wins" aggregation here.
function setMiniState(state) {
  $("mini-red").classList.toggle("on", !!(state && state.red));
  $("mini-orange").classList.toggle("on", !!(state && state.yellow));
  $("mini-green").classList.toggle("on", !!(state && state.green));
}

function renderLight() {
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
    label.textContent = snapshot.exists
      ? "Could not read state file"
      : "Waiting for clawlight…";
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

function renderDetail() {
  const list = $("sessions");
  list.replaceChildren();

  if (!snapshot || !snapshot.ok) {
    const empty = document.createElement("li");
    empty.className = "empty";
    empty.textContent = snapshot && snapshot.exists
      ? "State file could not be read."
      : "No state file yet. Use Settings to choose one.";
    list.appendChild(empty);
    return;
  }
  if (!snapshot.sessions.length) {
    const empty = document.createElement("li");
    empty.className = "empty";
    empty.textContent = "No sessions.";
    list.appendChild(empty);
    return;
  }

  for (const session of snapshot.sessions) {
    const li = document.createElement("li");
    li.className = `session ${STATUS_CLASS[session.status] || "gray"}`;

    const dot = document.createElement("span");
    dot.className = "status-dot";
    li.appendChild(dot);

    const meta = document.createElement("div");
    meta.className = "meta";

    const name = document.createElement("div");
    name.className = "name";
    name.textContent = session.name;
    name.title = session.name;
    meta.appendChild(name);

    const sub = document.createElement("div");
    sub.className = "sub";
    if (session.harness_badge) {
      const badge = document.createElement("span");
      badge.className = "badge";
      badge.textContent = session.harness_badge;
      sub.appendChild(badge);
    }
    const statusText = document.createElement("span");
    statusText.className = "status-text";
    statusText.textContent = session.status_label;
    sub.appendChild(statusText);
    if (session.project_name) {
      sub.appendChild(document.createTextNode(`· ${session.project_name}`));
    }
    const when = relativeTime(session.last_updated);
    if (when) {
      sub.appendChild(document.createTextNode(`· ${when}`));
    }
    meta.appendChild(sub);
    li.appendChild(meta);

    const remove = document.createElement("button");
    remove.className = "remove";
    remove.textContent = "×";
    remove.title = "Remove from tracking";
    remove.addEventListener("click", () => removeSession(session.session_id, session.name));
    li.appendChild(remove);

    list.appendChild(li);
  }

  $("detail-title").textContent =
    `${snapshot.sessions.length} session${snapshot.sessions.length === 1 ? "" : "s"}`;
}

function renderPin() {
  $("btn-pin").classList.toggle("active", !!(config && config.always_on_top));
}

function render() {
  renderLight();
  renderDetail();
  renderPin();
}

function populateSettings() {
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

async function refreshSnapshot() {
  if (!invoke) return;
  try {
    snapshot = await invoke("get_snapshot");
  } catch (error) {
    snapshot = { ok: false, exists: false, error: String(error), sessions: [], counts: {} };
  }
  render();
}

async function loadConfig() {
  if (!invoke) return;
  try {
    config = await invoke("get_config");
  } catch (error) {
    config = null;
  }
  renderPin();
  applyCollapseStyle();
}

async function removeSession(sessionId, name) {
  try {
    await invoke("clear_session", { sessionId });
    toast(`Removed “${name}”`);
  } catch (error) {
    toast(`Remove failed: ${error}`);
  }
}

async function clearDone() {
  try {
    const removed = await invoke("clear_done");
    toast(removed > 0 ? `Cleared ${removed} done` : "Nothing to clear");
  } catch (error) {
    toast(`Clear failed: ${error}`);
  }
}

async function saveSettings(event) {
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
    config = await invoke("set_config", { config: next });
    applyCollapseStyle();
    toast("Saved");
    await refreshSnapshot();
    setView("detail");
  } catch (error) {
    toast(`Save failed: ${error}`);
  }
}

function wireMini() {
  const mini = $("view-mini");
  let armed = false;
  let dragging = false;
  let startX = 0;
  let startY = 0;

  mini.addEventListener("mousedown", (event) => {
    if (event.button !== 0) return;
    armed = true;
    dragging = false;
    startX = event.screenX;
    startY = event.screenY;
  });
  window.addEventListener("mousemove", (event) => {
    if (!armed || dragging || !(event.buttons & 1)) return;
    if (Math.hypot(event.screenX - startX, event.screenY - startY) < 4) return;
    dragging = true;
    const win = getCurrentWindow ? getCurrentWindow() : null;
    if (win && win.startDragging) win.startDragging().catch(() => {});
  });
  const release = () => {
    if (armed && !dragging) setView("detail");
    armed = false;
    dragging = false;
  };
  window.addEventListener("mouseup", release);
  window.addEventListener("blur", () => {
    armed = false;
    dragging = false;
  });
}

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
      snapshot = event.payload;
      render();
    });
    listen("config-changed", (event) => {
      config = event.payload;
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
    if (view === "detail") renderDetail();
  }, 30000);
}

boot();
