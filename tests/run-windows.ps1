# Full Windows verification for AgentLight:
#   - Playwright: frontend (dist/) with a mocked __TAURI__, plus the real hub web UI
#   - server HTTP smoke: ingest / snapshot / removed-session tombstone / URL template
#   - optional UI Automation harness for native window behavior (interactive desktop)
#   - optional WebView2 CDP end-to-end against the real app window
#
# Usage (host):
#   powershell -ExecutionPolicy Bypass -File tests\run-windows.ps1
# Usage (sandbox, from the read-only mirror):
#   powershell -File C:\Workspace\AgentLight\tests\run-windows.ps1 `
#       -ReportDir C:\SandboxOutput\test-report -SkipUia
#
# Writes a report and exits non-zero if anything failed.
[CmdletBinding()]
param(
    [string]$RepoRoot = (Split-Path $PSScriptRoot -Parent),
    [string]$UiTestsDir = "",
    [string]$ReportDir = "",
    [string]$ServerExe = "",
    [string]$AppExe = "",
    [switch]$SkipUia,
    [switch]$SkipCdp
)

$ErrorActionPreference = "Continue"
if (-not $UiTestsDir) { $UiTestsDir = Join-Path $RepoRoot "tests\playwright" }
$WindowsTestsDir = Join-Path $RepoRoot "tests\windows"
if (-not $ReportDir) { $ReportDir = Join-Path $RepoRoot "tests\test-report" }
try { New-Item -ItemType Directory -Force -Path $ReportDir | Out-Null }
catch {
    # The sandbox's C:\Workspace mirror is read-only; fall back to a temp dir.
    $ReportDir = Join-Path $env:TEMP "agentlight-test-report"
    New-Item -ItemType Directory -Force -Path $ReportDir | Out-Null
}
$Report = Join-Path $ReportDir "windows.txt"
$script:Lines = New-Object System.Collections.Generic.List[string]
$script:Failed = 0

function Say($m) { $script:Lines.Add([string]$m); Write-Host $m }
function Step($name, [scriptblock]$body) {
    Say ""
    Say "== $name =="
    try { & $body } catch { Say ("EXCEPTION: " + $_.Exception.Message); $script:Failed = 1 }
}
function Check($ok, $okMsg, $failMsg) {
    if ($ok) { Say ("PASS  " + $okMsg) } else { Say ("FAIL  " + $failMsg); $script:Failed = 1 }
}

function Resolve-Exe([string]$explicit, [string[]]$candidates) {
    if ($explicit) { return (Resolve-Path $explicit).Path }
    foreach ($c in $candidates) { if ($c -and (Test-Path $c)) { return (Resolve-Path $c).Path } }
    return ""
}

$ServerExe = Resolve-Exe $ServerExe @(
    (Join-Path $RepoRoot "target\ci\agentlight-server.exe"),
    (Join-Path $RepoRoot "target\release\agentlight-server.exe"),
    "C:\SandboxOutput\build\agentlight-server.exe"
)
$AppExe = Resolve-Exe $AppExe @(
    (Join-Path $RepoRoot "target\ci\agentlight.exe"),
    (Join-Path $RepoRoot "target\release\agentlight.exe"),
    "C:\SandboxOutput\build\AgentLight-portable.exe"
)

Say "AgentLight Windows verification - $(Get-Date -Format s)"
Say "repo=$RepoRoot"
Say "server=$ServerExe"
Say "app=$AppExe"
Say "uiTests=$UiTestsDir"

$node = (Get-Command node -ErrorAction SilentlyContinue).Source
if (-not $node) { Say "FATAL: node not on PATH"; $script:Lines | Set-Content $Report; exit 1 }
$npmCli = Join-Path (Split-Path $node -Parent) "node_modules\npm\bin\npm-cli.js"

Step "playwright install" {
    $cli = Join-Path $UiTestsDir "node_modules\playwright\cli.js"
    if (-not (Test-Path $cli)) {
        Push-Location $UiTestsDir
        & $node $npmCli install --include=dev --no-audit --no-fund | Out-Null
        Pop-Location
    }
    & $node $cli install chromium | Out-Null
    Check (Test-Path $cli) "playwright present" "playwright cli still missing"
}

if ($ServerExe) { $env:AGENTLIGHT_SERVER = $ServerExe }
Step "playwright suite (frontend + web UI)" {
    $cli = Join-Path $UiTestsDir "node_modules\playwright\cli.js"
    Push-Location $UiTestsDir
    & $node $cli test 2>&1 | Tee-Object -Variable pw | Out-Null
    Pop-Location
    $pw | ForEach-Object { Say $_ }
    $text = ($pw | Out-String)
    Check ($text -match "(\d+) passed") "playwright suite passed" "playwright suite failed"
}

Step "server HTTP smoke" {
    if (-not $ServerExe) { Say "SKIP: no agentlight-server.exe found"; return }
    $tmp = Join-Path $env:TEMP ("agentlight-verify-" + [guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Force $tmp | Out-Null
    $port = 18811
    $base = "http://127.0.0.1:$port"
    $env:AGENTLIGHT_SOURCE = "events"
    $env:AGENTLIGHT_BIND = "127.0.0.1:$port"
    $env:AGENTLIGHT_TOKEN = ""
    $env:AGENTLIGHT_EVENTS_FILE = Join-Path $tmp "push.json"
    $env:AGENTLIGHT_DEVICES_FILE = Join-Path $tmp "devices.json"
    $env:AGENTLIGHT_SESSION_URL_TEMPLATE = "http://opencode-home/session/{id}"
    $srv = Start-Process -FilePath $ServerExe -PassThru -WindowStyle Hidden
    try {
        $ok = $false
        for ($i = 0; $i -lt 50 -and -not $ok; $i++) {
            Start-Sleep -Milliseconds 200
            try { $ok = (Invoke-RestMethod "$base/healthz" -TimeoutSec 2).schema_version -eq 1 } catch {}
        }
        Check $ok "healthz schema 1" "server did not become healthy"

        Invoke-RestMethod "$base/api/v1/ingest" -Method Post -ContentType "application/json" `
            -Body '{"events":[{"session_id":"a","status":"active","url":"http://localhost:4096/session/a","producer":"p1"}]}' | Out-Null
        $snap = Invoke-RestMethod "$base/api/v1/snapshot"
        Check ($snap.counts.total -eq 1) "ingest" "ingest failed"
        Check ($snap.sessions[0].url -eq "http://opencode-home/session/a") "server URL template" "url template failed ($($snap.sessions[0].url))"

        Invoke-RestMethod "$base/api/v1/commands" -Method Post -ContentType "application/json" `
            -Body '{"command":"remove_session","session_id":"a"}' | Out-Null
        Invoke-RestMethod "$base/api/v1/ingest" -Method Post -ContentType "application/json" `
            -Body '{"mode":"snapshot","events":[{"session_id":"a","status":"active","producer":"p1"}]}' | Out-Null
        $snap = Invoke-RestMethod "$base/api/v1/snapshot"
        Check ($snap.counts.total -eq 0) "removed session stays removed" "tombstone failed"
    } finally {
        Stop-Process -Id $srv.Id -Force -ErrorAction SilentlyContinue
        Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

if (-not $SkipUia) {
    Step "UI Automation harness (native window)" {
        $harness = Join-Path $WindowsTestsDir "ui-harness.ps1"
        if (-not (Test-Path $harness)) { Say "SKIP: ui-harness.ps1 not found"; return }
        $args = @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $harness,
                  "-OutDir", (Join-Path $ReportDir "ui"))
        if ($AppExe) { $args += @("-Exe", $AppExe) }
        & powershell.exe @args | ForEach-Object { Say $_ }
        $uiReport = Join-Path $ReportDir "ui\report.txt"
        if (Test-Path $uiReport) { Say "---- ui-harness report ----"; Get-Content $uiReport | ForEach-Object { Say $_ } }
    }
} else {
    Say ""
    Say "== UI Automation harness == skipped (-SkipUia); run on a real desktop."
}

if (-not $SkipCdp) {
    Step "WebView2 CDP e2e (real app UI)" {
        $runner = Join-Path $WindowsTestsDir "run-cdp.ps1"
        if (-not (Test-Path $runner)) { Say "SKIP: run-cdp.ps1 not found"; return }
        $cdpArgs = @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $runner, "-OutDir", (Join-Path $ReportDir "cdp"))
        if ($AppExe) { $cdpArgs += @("-Exe", $AppExe) }
        & powershell.exe @cdpArgs | ForEach-Object { Say $_ }
        Check ($LASTEXITCODE -eq 0) "CDP e2e passed" "CDP e2e failed (exit $LASTEXITCODE)"
    }
} else {
    Say ""
    Say "== WebView2 CDP e2e == skipped (-SkipCdp)"
}

Say ""
if ($script:Failed -eq 0) { Say "RESULT: ALL PASS" } else { Say "RESULT: FAILURES (see above)" }
$script:Lines | Set-Content $Report
Write-Host ("report: " + $Report)
exit $script:Failed
