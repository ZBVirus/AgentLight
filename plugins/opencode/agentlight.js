// AgentLight opencode plugin (reference implementation).
//
// Forwards opencode session state to an AgentLight hub running in events mode
// (`AGENTLIGHT_SOURCE=events`). It is an event-bus observer: it never changes
// opencode behavior, it only reports.
//
// Install: copy this file to `.opencode/plugins/agentlight.js` (project) or
// `~/.config/opencode/plugins/agentlight.js` (global). opencode loads any
// `*.js`/`*.ts` in those directories at startup.
//
// Environment (read from the opencode process):
//   AGENTLIGHT_HUB_URL  Hub base URL. Default http://127.0.0.1:8787
//   AGENTLIGHT_TOKEN    Bearer token, if the hub requires auth. Never logged.
//
// Mapping (opencode -> AgentLight `status`):
//   session.status busy / retry      -> active      (working)
//   session.status idle, session.idle-> inactive    (paused)
//   permission.asked / .updated      -> needs_help  (waiting on you)
//   permission.replied               -> active      (you answered, it resumes)
//   session.error                    -> needs_help
//   session.deleted                  -> done
//
// Status is forwarded with a short per-session coalescing window so bursts of
// events collapse into the latest state.
//
// NOTE: this targets the documented plugin API and the event names in
// `@opencode-ai/sdk` at the time of writing. It has not been run against a
// live opencode build in this repository. If an event name or payload differs
// in your version, adapt the `switch` in `handleEvent` below.

const DEFAULT_HUB_URL = "http://127.0.0.1:8787";
const COALESCE_MS = 150;
const HARNESS = "opencode";

export const AgentLightPlugin = async ({ directory, client }) => {
  const hubUrl = (process.env.AGENTLIGHT_HUB_URL || DEFAULT_HUB_URL).replace(/\/+$/, "");
  const token = process.env.AGENTLIGHT_TOKEN || "";
  const ingestUrl = `${hubUrl}/api/v1/ingest`;

  // sessionID -> { name, projectPath }
  const sessions = new Map();
  // sessionID -> { event, timer }
  const pending = new Map();

  const log = async (level, message, extra) => {
    try {
      await client.app.log({
        body: { service: "agentlight", level, message, extra },
      });
    } catch {
      if (level === "error") console.error(`agentlight: ${message}`, extra || "");
    }
  };

  const now = () => new Date().toISOString();

  const buildEvent = (sessionID, status) => {
    const known = sessions.get(sessionID) || {};
    return {
      session_id: sessionID,
      status,
      name: known.name || null,
      project_path: known.projectPath || directory || null,
      harness: HARNESS,
      last_updated: now(),
    };
  };

  const flush = async (sessionID) => {
    const entry = pending.get(sessionID);
    if (!entry) return;
    pending.delete(sessionID);
    const body = JSON.stringify({ events: [entry.event] });

    const headers = { "Content-Type": "application/json" };
    if (token) headers.Authorization = `Bearer ${token}`;

    try {
      const response = await fetch(ingestUrl, { method: "POST", headers, body });
      if (!response.ok) {
        const text = await response.text().catch(() => "");
        await log("warn", `hub rejected event for ${sessionID}`, {
          status: response.status,
          body: text.slice(0, 200),
        });
      }
    } catch (error) {
      await log("error", `could not reach hub at ${hubUrl}`, {
        error: String(error && error.message ? error.message : error),
      });
    }
  };

  const report = (sessionID, status) => {
    if (!sessionID) return;
    const event = buildEvent(sessionID, status);
    const existing = pending.get(sessionID);
    if (existing) clearTimeout(existing.timer);
    const timer = setTimeout(() => {
      flush(sessionID);
    }, COALESCE_MS);
    // Allow opencode to exit without waiting on the timer.
    if (timer && typeof timer.unref === "function") timer.unref();
    pending.set(sessionID, { event, timer });
  };

  const remember = (info) => {
    if (!info || !info.id) return;
    sessions.set(info.id, {
      name: info.title || null,
      projectPath: info.directory || null,
    });
  };

  const handleEvent = async (event) => {
    const props = event.properties || {};
    switch (event.type) {
      case "session.created":
      case "session.updated":
        remember(props.info);
        break;
      case "session.deleted":
        if (props.info) {
          remember(props.info);
          report(props.info.id, "done");
        } else if (props.sessionID) {
          report(props.sessionID, "done");
        }
        break;
      case "session.status": {
        const status = props.status && props.status.type;
        report(props.sessionID, status === "idle" ? "inactive" : "active");
        break;
      }
      case "session.idle":
        report(props.sessionID, "inactive");
        break;
      case "session.error":
        report(props.sessionID, "needs_help");
        break;
      // opencode's SDK has used both names for the "waiting for approval" event.
      case "permission.asked":
      case "permission.updated":
        report(props.sessionID, "needs_help");
        break;
      case "permission.replied":
        report(props.sessionID, "active");
        break;
      default:
        break;
    }
  };

  return {
    event: async ({ event }) => {
      try {
        await handleEvent(event);
      } catch (error) {
        await log("error", "failed to handle opencode event", {
          type: event && event.type,
          error: String(error && error.message ? error.message : error),
        });
      }
    },
    "tool.execute.before": async ({ sessionID }) => {
      report(sessionID, "active");
    },
    "tool.execute.after": async ({ sessionID }) => {
      report(sessionID, "active");
    },
  };
};
