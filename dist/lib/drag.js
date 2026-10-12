"use strict";

import { getCurrentWindow, invoke } from "./ipc.js";
import { setView, view } from "./store.js";

const $ = (id) => document.getElementById(id);

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
