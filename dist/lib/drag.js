"use strict";

import { getCurrentWindow, invoke } from "./ipc.js";
import { applyCollapseStyle, collapseStyle, config, setConfig, setView, view } from "./store.js";

const $ = (id) => document.getElementById(id);

const MENU_ID = "mini-context-menu";
let menuOpen = false;

// Collapsed-view layouts offered in the right-click menu, in the order shown.
const LAYOUTS = [
  ["single", "Single light"],
  ["triple", "Horizontal lights"],
  ["triple_vertical", "Vertical lights"],
];

function closeMenu() {
  const menu = document.getElementById(MENU_ID);
  if (menu) menu.classList.add("hidden");
  menuOpen = false;
}

function menuItem(label, onClick) {
  const item = document.createElement("button");
  item.type = "button";
  item.className = "context-item";
  item.setAttribute("role", "menuitem");
  item.textContent = label;
  item.addEventListener("click", onClick);
  return item;
}

// Switch the collapsed layout from the context menu, persisting the choice.
function applyLayout(style) {
  closeMenu();
  if (!invoke || !config) return;
  invoke("set_config", { config: { ...config, collapse_style: style } })
    .then((next) => {
      if (next) setConfig(next);
      applyCollapseStyle();
    })
    .catch(() => {});
}

function ensureMenu() {
  let menu = document.getElementById(MENU_ID);
  if (!menu) {
    menu = document.createElement("div");
    menu.id = MENU_ID;
    menu.className = "context-menu hidden";
    menu.setAttribute("role", "menu");
    document.body.appendChild(menu);
  }
  // Rebuild each time so the check mark tracks the current layout.
  menu.replaceChildren();
  const current = collapseStyle();
  for (const [value, label] of LAYOUTS) {
    const mark = value === current ? "\u2713 " : "\u2003";
    menu.appendChild(menuItem(`${mark}${label}`, () => applyLayout(value)));
  }
  const separator = document.createElement("div");
  separator.className = "context-sep";
  menu.appendChild(separator);
  menu.appendChild(
    menuItem("Hide", () => {
      closeMenu();
      if (invoke) invoke("window_hide").catch(() => {});
    }),
  );
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
