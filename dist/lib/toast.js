"use strict";

const $ = (id) => document.getElementById(id);
let toastTimer = null;

export function toast(message) {
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
