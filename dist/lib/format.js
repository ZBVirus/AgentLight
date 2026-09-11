"use strict";

export const MINI_SIZES = {
  single: { width: 88, height: 88 },
  triple: { width: 172, height: 68 },
  triple_vertical: { width: 68, height: 172 },
};

export const SIZES = {
  detail: { width: 420, height: 548 },
  settings: { width: 420, height: 600 },
};

export const AGG_LABEL = {
  red: "Needs input",
  orange: "Idle",
  green: "All running",
  gray: "No sessions",
};

export const STATUS_CLASS = {
  needs_help: "red",
  active: "green",
  inactive: "orange",
  done: "gray",
};

export function relativeTime(iso) {
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
