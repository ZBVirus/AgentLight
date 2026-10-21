[CmdletBinding()]
param(
    [string]$Exe = "",
    [int]$Port = 9222,
    [string]$OutDir = "",
    [int]$WaitMs = 25000
)

$ErrorActionPreference = "Stop"
$repo = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent

if (-not $OutDir) { $OutDir = Join-Path $repo "tests\test-report\cdp" }
try { New-Item -ItemType Directory -Force -Path $OutDir | Out-Null }
catch {
    # The sandbox's C:\Workspace mirror is read-only; fall back to a temp dir.
    $OutDir = Join-Path $env:TEMP "agentlight-cdp"
    New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
}
$report = Join-Path $OutDir "report.txt"

if (-not $Exe) {
    foreach ($c in @(
            (Join-Path $repo "target\ci\agentlight.exe"),
            (Join-Path $repo "target\release\agentlight.exe"),
            "C:\SandboxOutput\build\AgentLight-portable.exe"
        )) { if (Test-Path $c) { $Exe = (Resolve-Path $c).Path; break } }
}
if (-not (Test-Path $Exe)) { throw "agentlight.exe not found; pass -Exe <path>" }

$node = (Get-Command node -ErrorAction SilentlyContinue).Source
if (-not $node) { throw "node not on PATH" }
$npm = Join-Path (Split-Path $node -Parent) "npm.cmd"

# @playwright/test must be resolvable next to the script. CI installs it in
# tests/windows; the read-only sandbox mirror cannot, so use a writable copy in
# %TEMP% (installed once per session) and run the suite there.
$scriptDir = $PSScriptRoot
if (-not (Test-Path (Join-Path $scriptDir "node_modules\@playwright\test"))) {
    $work = Join-Path $env:TEMP "agentlight-cdp"
    New-Item -ItemType Directory -Force -Path $work | Out-Null
    Copy-Item (Join-Path $scriptDir "e2e-cdp.js") $work -Force
    if (-not (Test-Path (Join-Path $work "node_modules\@playwright\test"))) {
        Copy-Item (Join-Path $scriptDir "package.json") $work -Force
        Push-Location $work
        & $npm install --include=dev --no-audit --no-fund 2>&1 | Out-Null
        Pop-Location
    }
    $scriptDir = $work
}

Get-Process -Name 'AgentLight*' -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 1

# The build must carry additionalBrowserArgs=--remote-debugging-port=<Port>;
# wry overrides WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS, so the env var alone is
# not reliable. It is set anyway for builds that do honour it.
$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=$Port --remote-allow-origins=*"
$app = Start-Process -FilePath $Exe -PassThru
$lines = New-Object System.Collections.Generic.List[string]
$code = 1
try {
    $deadline = (Get-Date).AddMilliseconds($WaitMs)
    $ready = $false
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 300
        try {
            Invoke-WebRequest -Uri "http://127.0.0.1:$Port/json/version" -UseBasicParsing -TimeoutSec 2 | Out-Null
            $ready = $true; break
        } catch {}
    }
    if (-not $ready) {
        $lines.Add("FAIL  WebView2 CDP did not start on port $Port")
        $lines.Add("      build with additionalBrowserArgs=--remote-debugging-port=$Port (see README)")
    } else {
        $lines.Add("CDP up on $Port; running e2e-cdp.js")
        $env:AGENTLIGHT_CDP_PORT = "$Port"
        $out = & $node (Join-Path $scriptDir "e2e-cdp.js") 2>&1
        $code = $LASTEXITCODE
        $out | ForEach-Object { $lines.Add([string]$_) }
    }
} catch {
    $lines.Add("EXCEPTION: " + $_.Exception.ToString())
    $code = 1
} finally {
    Get-Process -Name 'AgentLight*' -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    $lines | Set-Content -Encoding UTF8 $report
    Write-Host ("report: " + $report)
}
exit $code
