const { defineConfig } = require("@playwright/test");

module.exports = defineConfig({
  testDir: "./tests",
  timeout: 30000,
  expect: { timeout: 5000 },
  reporter: [["list"]],
  use: {
    baseURL: "http://127.0.0.1:4319",
    viewport: { width: 800, height: 900 },
    deviceScaleFactor: 1,
  },
  webServer: {
    command: "node static-server.js /workspace/AgentLight/dist 4319",
    url: "http://127.0.0.1:4319/index.html",
    reuseExistingServer: true,
    cwd: __dirname,
  },
});
