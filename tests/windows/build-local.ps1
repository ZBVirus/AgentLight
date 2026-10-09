# Build the AgentLight GUI for GUI testing.
#
# Build on the sandbox's LOCAL system disk (C:\AL-build). The mapped folders
# (C:\SandboxOutput and even C:\Persistent) fail with os error 1392 when
# executing freshly built scripts, so the source copy and target cannot live on
# them. As a result the build is ephemeral and must be redone after a sandbox
# restart; the cargo registry cache under C:\Persistent\Caches still persists.
#
# The test build enables WebView2 remote debugging (additionalBrowserArgs) so
# tests\windows\run-cdp.ps1 can drive the real UI.
#
# Run inside the sandbox: powershell -File C:\Workspace\AgentLight\tests\windows\build-local.ps1
$ErrorActionPreference = 'Continue'
$src = 'C:\Workspace\AgentLight'
$work = 'C:\AL-build'
$targetDir = 'C:\AL-build\target'
$log = 'C:\Temp\agentlight-build.log'
$cdpPort = 9222

"== build start $(Get-Date -Format s) ==" | Tee-Object -FilePath $log
"work=$work  target=$targetDir  TEMP=$env:TEMP" | Tee-Object -Append -FilePath $log

if (Test-Path $work) { Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue }
New-Item -ItemType Directory -Force -Path $work | Out-Null
robocopy $src $work /E /XD target .git node_modules local /NFL /NDL /NJH /NJS /NP | Out-Null
Get-ChildItem $work -Recurse -File -ErrorAction SilentlyContinue | ForEach-Object { $_.IsReadOnly = $false }

# Test build: enable WebView2 remote debugging for the CDP end-to-end suite.
$conf = Join-Path $work 'src-tauri\tauri.conf.json'
node -e "const fs=require('fs');const p=process.argv[1];const j=JSON.parse(fs.readFileSync(p,'utf8'));j.app.windows[0].additionalBrowserArgs='--remote-debugging-port=$cdpPort --remote-allow-origins=*';fs.writeFileSync(p,JSON.stringify(j,null,2));" $conf
if ($LASTEXITCODE -ne 0) { "FAILED to patch tauri.conf.json for CDP" | Tee-Object -Append -FilePath $log } else { "cdp port $cdpPort enabled" | Tee-Object -Append -FilePath $log }

Set-Location $work
$env:CARGO_TARGET_DIR = $targetDir
"cargo: $((Get-Command cargo -ErrorAction SilentlyContinue).Source)" | Tee-Object -Append -FilePath $log
cargo build -p agentlight --profile ci 2>&1 | Tee-Object -Append -FilePath $log
"cargo exit: $LASTEXITCODE" | Tee-Object -Append -FilePath $log

$exe = Join-Path $targetDir 'ci\agentlight.exe'
if (Test-Path $exe) {
    Copy-Item $exe 'C:\SandboxOutput\build\AgentLight-portable.exe' -Force
    $item = Get-Item $exe
    "exe: $($item.FullName) $($item.Length) bytes $($item.LastWriteTime)" | Tee-Object -Append -FilePath $log
} else {
    "EXE NOT BUILT" | Tee-Object -Append -FilePath $log
}
"== build done $(Get-Date -Format s) ==" | Tee-Object -Append -FilePath $log
