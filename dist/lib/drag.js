"use strict";

import { getCurrentWindow, invoke } from "./ipc.js";
import { setView, view } from "./store.js";

const $ = (id) => document.getElementById(id);

// Whether the last reported state was "dragging". Only changes are sent, so a
// burst of mousedowns/mouseups cannot spam the shell.
let dragReported = false;

// Tell the shell a drag started or ended. The shell skips the outside-click
// collapse while a drag is fresh, which keeps dragging safe without deferring
// real outside clicks.
function reportDragging(next) {
  if (next === dragReported) return;
  dragReported = next;
  if (invoke) invoke("set_dragging", { dragging: next }).catch(() => {});
}

// `closest` without assuming the event target's realm, so the jsdom tests work
// with swapped documents.
function closest(element, selector) {
  return element && typeof element.closest === "function"
    ? element.closest(selector)
    : null;
}

export function wireMini() {
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

  // Any drag region (the title bars) or the collapsed light starts a drag on
  // the primary button; report both ends so the shell can tell a drag from a
  // click away without a timeout.
  document.addEventListener("mousedown", (event) => {
    if (event.button !== 0) return;
    if (
      closest(event.target, "[data-tauri-drag-region]") ||
      closest(event.target, "#view-mini")
    ) {
      reportDragging(true);
    }
  });
  window.addEventListener("mouseup", () => reportDragging(false));

  // Right-click opens a native layout/Hide menu. The OS draws it, so it can
  // extend past this small window instead of being clipped by it.
  mini.addEventListener("contextmenu", (event) => {
    if (view !== "mini") return;
    event.preventDefault();
    armed = false;
    dragging = false;
    if (invoke) invoke("show_mini_menu").catch(() => {});
  });
}
