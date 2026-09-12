"use strict";

import { invoke } from "../lib/ipc.js";
import { STATUS_CLASS, relativeTime } from "../lib/format.js";
import { toast } from "../lib/toast.js";

const $ = (id) => document.getElementById(id);

export function renderDetail(snapshot) {
  const list = $("sessions");
  list.replaceChildren();

  if (!snapshot || !snapshot.ok) {
    const empty = document.createElement("li");
    empty.className = "empty";
    if (snapshot && snapshot.error) {
      empty.textContent = snapshot.error;
    } else if (snapshot && snapshot.source_kind === "hub") {
      empty.textContent = snapshot.exists
        ? "Could not reach the hub."
        : "Not connected to a hub. Check Settings.";
    } else {
      empty.textContent = snapshot && snapshot.exists
        ? "State file could not be read."
        : "No state file yet. Use Settings to choose one.";
    }
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

export async function removeSession(sessionId, name) {
  try {
    await invoke("clear_session", { sessionId });
    toast(`Removed “${name}”`);
  } catch (error) {
    toast(`Remove failed: ${error}`);
  }
}

export async function clearDone() {
  try {
    const removed = await invoke("clear_done");
    toast(removed > 0 ? `Cleared ${removed} done` : "Nothing to clear");
  } catch (error) {
    toast(`Clear failed: ${error}`);
  }
}
