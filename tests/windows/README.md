# Windows GUI tests

Tests that need the real Tauri/WebView2 window. Two layers:

- `ui-harness.ps1` — native window behavior via Win32 + UI Automation: title,
  collapsed size (88x88 / 172x68 / 68x172), fixed size (`WS_THICKFRAME=false`),
  topmost (`WS_EX_TOPMOST`), z-order, no console child, AUMID. Needs an
  interactive desktop; it cannot click the WebView2 DOM (UIA does not surface it).
- `e2e-cdp.js` (run via `run-cdp.ps1`) — Playwright over WebView2 remote
  debugging: real DOM interactions plus the real Rust window resizes
  (88 <-> 420x548), the pin/always-on-top toggle, settings gating, and
  non-resizability.

`run-cdp.ps1` starts the app and connects Playwright over CDP; the build must
carry `additionalBrowserArgs: "--remote-debugging-port=9222 --remote-allow-origins=*"`.

## Running

```powershell
# all Windows suites (Playwright + server smoke + harness + CDP)
powershell -ExecutionPolicy Bypass -File tests\run-windows.ps1

# just the CDP suite (build must already have the CDP arg)
powershell -ExecutionPolicy Bypass -File tests\windows\run-cdp.ps1 -Exe <path-to-agentlight.exe>
```

CI runs `tests\windows\run-cdp.ps1` in the `windows-gui` job (windows-latest),
patching `src-tauri\tauri.conf.json` with the CDP arg before building.

## Windows Sandbox

The sandbox has an interactive desktop in **Session 1** (`WDAGUtilityAccount`,
`explorer` + `winlogon`); SSH lands in **Session 0**. Launch GUI apps into
Session 1 with **PsExec**:

```powershell
C:\Persistent\Tools\PSTools\PsExec64.exe -accepteula -i 1 -d powershell.exe `
  -NoProfile -ExecutionPolicy Bypass -File C:\Workspace\AgentLight\tests\windows\ui-harness.ps1 `
  -Exe C:\AL-build\target\ci\agentlight.exe -OutDir C:\SandboxOutput\ui-report
```

Provisioning (lost when the sandbox restarts; re-run):

```powershell
# idempotent: installs only what is missing (SDK/MSVC, VC++ runtime, WebView2)
powershell -File C:\Workspace\AgentLight\tests\windows\setup-toolchain.ps1
```

`build-local.ps1` calls it automatically before building, so a fresh sandbox
needs no manual step.

Build on the sandbox's local disk. Both mapped folders (`C:\SandboxOutput` and
`C:\Persistent`) fail with `os error 1392` when executing freshly built scripts,
so the build is ephemeral and must be redone after a sandbox restart; the cargo
registry cache under `C:\Persistent\Caches` still persists.

```powershell
powershell -File C:\Workspace\AgentLight\tests\windows\build-local.ps1
# -> C:\AL-build\target\ci\agentlight.exe (CDP-enabled test build)
```

Then run `run-cdp.ps1` / `ui-harness.ps1` against that exe.

## Gotchas

- Always kill by wildcard: `Get-Process -Name 'AgentLight*' | Stop-Process -Force`.
  A leftover `AgentLight-portable.exe` holds the `com.zbvirus.agentlight-sic`
  single-instance lock and the next launch exits immediately.
- `wry` overrides `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`, so CDP must come from
  `additionalBrowserArgs` in `tauri.conf.json` (test builds only).
- `CopyFromScreen` fails without a visible console; the harness captures with
  `PrintWindow` instead.
