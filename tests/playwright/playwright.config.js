const { defineConfig } = require("@playwright/test");

// Browser channel. Default: Playwright's bundled Chromium (CI installs it).
// Set PLAYWRIGHT_CHANNEL=msedge (or chrome) to drive a system browser instead,
// which needs no download — the way to run this suite on a dev machine or the
// Windows sandbox where Playwright's browser download is unavailable.
const channel = process.env.PLAYWRIGHT_CHANNEL || undefined;

module.exports = defineConfig({
  testDir: "./tests",
  timeout: 30000,
  expect: { timeout: 5000 },
  reporter: [["list"]],
  use: {
    baseURL: "http://127.0.0.1:4319",
    viewport: { width: 800, height: 900 },
    deviceScaleFactor: 1,
    ...(channel ? { channel } : {}),
  },
  webServer: {
    command: "node static-server.js",
    url: "http://127.0.0.1:4319/index.html",
    reuseExistingServer: true,
    cwd: __dirname,
  },
});
