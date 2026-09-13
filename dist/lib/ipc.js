"use strict";

// With `withGlobalTauri: true`, Tauri exposes its JS API on window.__TAURI__.
// Everything else (file IO, parsing, writes) happens in Rust commands.
const tauri = window.__TAURI__ || {};
export const invoke = tauri.core ? tauri.core.invoke : null;
export const listen = tauri.event ? tauri.event.listen : null;
export const getCurrentWindow = tauri.window ? tauri.window.getCurrentWindow : null;
