// Minimal static file server for the AgentLight dist/ frontend.
const http = require("http");
const fs = require("fs");
const path = require("path");

const root = process.argv[2] || "/workspace/AgentLight/dist";
const port = Number(process.argv[3] || 4319);
const types = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json",
  ".png": "image/png",
  ".svg": "image/svg+xml",
};

http
  .createServer((req, res) => {
    let rel = decodeURIComponent(req.url.split("?")[0]);
    if (rel === "/") rel = "/index.html";
    const file = path.normalize(path.join(root, rel));
    if (!file.startsWith(root)) {
      res.writeHead(403);
      res.end("forbidden");
      return;
    }
    fs.readFile(file, (err, data) => {
      if (err) {
        res.writeHead(404);
        res.end("not found");
        return;
      }
      res.writeHead(200, { "Content-Type": types[path.extname(file)] || "application/octet-stream" });
      res.end(data);
    });
  })
  .listen(port, "127.0.0.1", () => console.log(`serving ${root} on http://127.0.0.1:${port}`));
