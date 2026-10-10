#!/usr/bin/env bash
# Full Linux/container verification for AgentLight: Rust fmt/clippy/tests,
# plugin tests, frontend syntax, and an end-to-end run of the server HTTP/SSE API.
#
# Usage: bash tests/run-linux.sh
# Writes: tests/test-report/linux.txt
set -uo pipefail

export PATH="$HOME/.cargo/bin:$PATH"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$REPO/tests/test-report"
mkdir -p "$OUT"
REPORT="$OUT/linux.txt"
: > "$REPORT"

fail=0
report() { echo "$*" | tee -a "$REPORT"; }
step() {
  local name="$1"; shift
  if "$@" >>"$REPORT" 2>&1; then
    report "PASS  $name"
  else
    report "FAIL  $name"
    fail=1
  fi
}

report "AgentLight Linux verification — $(date -Iseconds)"
report "repo: $REPO"

cd "$REPO"

report ""
report "== Rust =="
step "cargo fmt --check" cargo fmt --all --check
step "cargo clippy" cargo clippy \
  -p agentlight-core -p agentlight-server -p agentlight-hub-client -p agentlight-source-events \
  --all-targets -- -D warnings
step "cargo test" cargo test \
  -p agentlight-core -p agentlight-server -p agentlight-hub-client -p agentlight-source-events

report ""
report "== Frontend / plugin =="
step "plugin node tests" node --test tests/plugin/
js_ok=1
while IFS= read -r f; do
  node --check "$f" >>"$REPORT" 2>&1 || js_ok=0
done < <(find dist plugins -name '*.js')
if [ "$js_ok" -eq 1 ]; then report "PASS  node --check dist/plugins"; else report "FAIL  node --check dist/plugins"; fail=1; fi

report ""
report "== Frontend (jsdom, no browser) =="
if [ ! -d "$REPO/tests/frontend/node_modules" ]; then
  (cd "$REPO/tests/frontend" && npm install --include=dev --no-audit --no-fund) >>"$REPORT" 2>&1
fi
if (cd "$REPO/tests/frontend" && node --test) >>"$REPORT" 2>&1; then
  report "PASS  frontend node --test"
else
  report "FAIL  frontend node --test"
  fail=1
fi

report ""
report "== Shell (staged sysroot) =="
if [ -d "$HOME/sysroot" ]; then
  if SYSROOT="$HOME/sysroot" \
     PKG_CONFIG_LIBDIR="$HOME/sysroot/usr/lib/pkgconfig:$HOME/sysroot/usr/share/pkgconfig" \
     PKG_CONFIG_SYSROOT_DIR="$HOME/sysroot" PKG_CONFIG_PATH="" \
     cargo check -p agentlight >>"$REPORT" 2>&1; then
    report "PASS  cargo check -p agentlight"
  else
    report "FAIL  cargo check -p agentlight"
    fail=1
  fi
else
  report "SKIP  cargo check -p agentlight (no ~/sysroot)"
fi

report ""
report "== Server end-to-end =="
if ! cargo build -p agentlight-server --quiet >>"$REPORT" 2>&1; then
  report "FAIL  build agentlight-server"
  fail=1
else
  SERVER="$REPO/target/debug/agentlight-server"
  TMP="$(mktemp -d)"
  PORT="${AGENTLIGHT_TEST_PORT:-18799}"
  BASE="http://127.0.0.1:$PORT"
  AGENTLIGHT_SOURCE=events AGENTLIGHT_BIND="127.0.0.1:$PORT" AGENTLIGHT_TOKEN= \
    AGENTLIGHT_EVENTS_FILE="$TMP/push.json" AGENTLIGHT_DEVICES_FILE="$TMP/devices.json" \
    "$SERVER" >"$TMP/server.log" 2>&1 &
  SRV_PID=$!
  for _ in $(seq 1 50); do curl -sf "$BASE/healthz" >/dev/null 2>&1 && break; sleep 0.2; done

  je() { python3 -c "import json,sys; d=json.load(sys.stdin); print(d$1)" 2>/dev/null; }

  health=$(curl -sf "$BASE/healthz")
  if [ "$(echo "$health" | je "['schema_version']")" = "1" ]; then report "PASS  healthz schema_version=1"; else report "FAIL  healthz"; fail=1; fi

  curl -sf -X POST -H 'Content-Type: application/json' \
    -d '{"events":[{"session_id":"a","status":"active","producer":"p1"}]}' "$BASE/api/v1/ingest" >/dev/null
  if [ "$(curl -sf "$BASE/api/v1/snapshot" | je "['counts']['total']")" = "1" ]; then report "PASS  ingest upsert"; else report "FAIL  ingest upsert"; fail=1; fi

  curl -sf -X POST -H 'Content-Type: application/json' \
    -d '{"command":"remove_session","session_id":"a"}' "$BASE/api/v1/commands" >/dev/null
  curl -sf -X POST -H 'Content-Type: application/json' \
    -d '{"mode":"snapshot","events":[{"session_id":"a","status":"active","producer":"p1"}]}' "$BASE/api/v1/ingest" >/dev/null
  if [ "$(curl -sf "$BASE/api/v1/snapshot" | je "['counts']['total']")" = "0" ]; then report "PASS  removed session stays removed"; else report "FAIL  tombstone"; fail=1; fi

  curl -sf -X POST -H 'Content-Type: application/json' \
    -d '{"events":[{"session_id":"a","status":"needs_help","producer":"p1"}]}' "$BASE/api/v1/ingest" >/dev/null
  if [ "$(curl -sf "$BASE/api/v1/snapshot" | je "['counts']['needs_help']")" = "1" ]; then report "PASS  real event resurrects"; else report "FAIL  resurrect"; fail=1; fi

  kill "$SRV_PID" 2>/dev/null
  wait "$SRV_PID" 2>/dev/null

  # URL template + producer-less no-prune need a fresh server.
  PORT2=$((PORT + 1))
  BASE2="http://127.0.0.1:$PORT2"
  AGENTLIGHT_SOURCE=events AGENTLIGHT_BIND="127.0.0.1:$PORT2" AGENTLIGHT_TOKEN= \
    AGENTLIGHT_SESSION_URL_TEMPLATE="http://opencode-home/session/{id}" \
    AGENTLIGHT_EVENTS_FILE="$TMP/push2.json" AGENTLIGHT_DEVICES_FILE="$TMP/devices2.json" \
    "$SERVER" >"$TMP/server2.log" 2>&1 &
  SRV2=$!
  for _ in $(seq 1 50); do curl -sf "$BASE2/healthz" >/dev/null 2>&1 && break; sleep 0.2; done
  curl -sf -X POST -H 'Content-Type: application/json' \
    -d '{"events":[{"session_id":"x","status":"active","url":"http://localhost:4096/session/x","producer":"p1"}]}' "$BASE2/api/v1/ingest" >/dev/null
  url="$(curl -sf "$BASE2/api/v1/snapshot" | je "['sessions'][0]['url']")"
  if [ "$url" = "http://opencode-home/session/x" ]; then report "PASS  server URL template"; else report "FAIL  server URL template ($url)"; fail=1; fi

  curl -sf -X POST -H 'Content-Type: application/json' \
    -d '{"events":[{"session_id":"y","status":"active"}]}' "$BASE2/api/v1/ingest" >/dev/null
  curl -sf -X POST -H 'Content-Type: application/json' \
    -d '{"mode":"snapshot","events":[{"session_id":"x","status":"active","producer":"p1"}]}' "$BASE2/api/v1/ingest" >/dev/null
  if [ "$(curl -sf "$BASE2/api/v1/snapshot" | je "['counts']['total']")" = "2" ]; then report "PASS  producer-less snapshot does not prune"; else report "FAIL  producer-less prune"; fail=1; fi

  kill "$SRV2" 2>/dev/null
  wait "$SRV2" 2>/dev/null
  rm -rf "$TMP"
fi

report ""
if [ "$fail" -eq 0 ]; then
  report "RESULT: ALL PASS"
else
  report "RESULT: FAILURES (see above)"
fi
exit "$fail"
