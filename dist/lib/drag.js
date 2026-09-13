"use strict";

import { getCurrentWindow, invoke } from "./ipc.js";
import { setView, view } from "./store.js";

const $ = (id) => document.getElementById(id);

const MENU_ID = "mini-context-menu";
let menuOpen = false;

function closeMenu() {
  const menu = document.getElementById(MENU_ID);
  if (menu) menu.classList.add("hidden");
  menuOpen = false;
}

function ensureMenu() {
  let menu = document.getElementById(MENU_ID);
  if (menu) return menu;
  menu = document.createElement("div");
  menu.id = MENU_ID;
  menu.className = "context-menu hidden";
  menu.setAttribute("role", "menu");
  const hide = document.createElement("button");
  hide.type = "button";
  hide.className = "context-item";
  hide.setAttribute("role", "menuitem");
  hide.textContent = "Hide";
  hide.addEventListener("click", () => {
    closeMenu();
    if (invoke) invoke("window_hide").catch(() => {});
  });
  menu.appendChild(hide);
  document.body.appendChild(menu);
  return menu;
}

function openMenu(x, y) {
  const menu = ensureMenu();
  menu.classList.remove("hidden");
  const { width, height } = menu.getBoundingClientRect();
  const left = Math.max(0, Math.min(x, window.innerWidth - width));
  const top = Math.max(0, Math.min(y, window.innerHeight - height));
  menu.style.left = `${left}px`;
  menu.style.top = `${top}px`;
  menuOpen = true;
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
    closeMenu();
  });

  // Right-click on the collapsed light offers "Hide", mirroring the tray.
  mini.addEventListener("contextmenu", (event) => {
    if (view !== "mini") return;
    event.preventDefault();
    armed = false;
    dragging = false;
    openMenu(event.clientX, event.clientY);
  });

  window.addEventListener("mousedown", (event) => {
    if (!menuOpen) return;
    const menu = document.getElementById(MENU_ID);
    if (menu && menu.contains(event.target)) return;
    closeMenu();
  });
  window.addEventListener("keydown", (event) => {
    if (menuOpen && event.key === "Escape") closeMenu();
  });
  window.addEventListener("scroll", closeMenu, true);
  window.addEventListener("resize", closeMenu);
}
