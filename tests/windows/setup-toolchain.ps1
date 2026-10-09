# Provision the Windows GUI toolchain AgentLight needs, on demand.
#
# Idempotent: each component is checked and only installed when missing, so it is
# fast when everything is already present. build-local.ps1 calls this
# automatically before building; run it directly to pre-provision a fresh
# sandbox.
#
# Installs to system locations that the sandbox discards on close, so it must be
# re-run after a restart (the installers/packages are cached under
# C:\Persistent\Caches so nothing is re-downloaded). Installers are executed from
# local scratch: the mapped persistent folders cannot execute freshly written
# binaries (os error 1392).
[CmdletBinding()]
param(
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
$Tools = 'C:\Persistent\Tools'
$Caches = 'C:\Persistent\Caches'
$Installers = Join-Path $Caches 'Installers'
$VSCache = Join-Path $Caches 'VSPackages'
$Scratch = Join-Path $env:TEMP 'agentlight-toolchain'
foreach ($d in @($Tools, $Installers, $VSCache, $Scratch)) {
    New-Item -ItemType Directory -Force -Path $d | Out-Null
}

function Test-Sdk {
    [bool](Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\Lib\*\um\x64\user32.lib' -ErrorAction SilentlyContinue | Select-Object -First 1)
}
function Test-VcRuntime {
    Test-Path (Join-Path $env:SystemRoot 'System32\vcruntime140.dll')
}
function Test-WebView2Files {
    [bool](Get-ChildItem 'C:\Program Files (x86)\Microsoft\EdgeWebView\Application\*\msedgewebview2.exe' -ErrorAction SilentlyContinue | Select-Object -First 1)
}
$WvGuid = '{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
$WvWow = "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\$WvGuid"
$WvNative = "HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\$WvGuid"

function Get-Installer([string]$Url, [string]$Name) {
    $cache = Join-Path $Installers $Name
    if (-not (Test-Path $cache)) { Invoke-WebRequest $Url -OutFile $cache -UseBasicParsing }
    $run = Join-Path $Scratch $Name
    Copy-Item $cache $run -Force
    return $run
}
function Wait-Until([scriptblock]$Test, [int]$Seconds, [string]$What) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while (-not (& $Test) -and (Get-Date) -lt $deadline) { Start-Sleep -Seconds 3 }
    if (-not (& $Test)) { throw "$What is still missing after $Seconds s" }
}

$changed = $false

# 1. Windows SDK + MSVC (to build the Tauri app). The VS bootstrapper can detach,
#    and a wedged installer state returns 0 without installing, so retry once and
#    then poll/verify.
if ($Force -or -not (Test-Sdk)) {
    Write-Host 'Provisioning Windows SDK / MSVC (VCTools)...' -ForegroundColor Yellow
    $vs = Join-Path $Tools 'vs_buildtools.exe'
    if (-not (Test-Path $vs)) { $vs = Get-Installer 'https://aka.ms/vs/17/release/vs_BuildTools.exe' 'vs_buildtools.exe' }
    $vsArgs = @('--quiet', '--wait', '--norestart', '--cache', $VSCache, '--add', 'Microsoft.VisualStudio.Workload.VCTools', '--includeRecommended')
    for ($attempt = 1; $attempt -le 2 -and -not (Test-Sdk); $attempt++) {
        if ($attempt -gt 1) { Write-Host 'retrying the SDK install...' -ForegroundColor Yellow; Start-Sleep -Seconds 5 }
        $p = Start-Process -FilePath $vs -ArgumentList $vsArgs -PassThru -Wait
        Write-Host ("vs_buildtools exit={0}" -f $p.ExitCode)
        $deadline = (Get-Date).AddSeconds(90)
        while (-not (Test-Sdk) -and (Get-Date) -lt $deadline) { Start-Sleep -Seconds 5 }
    }
    Wait-Until { Test-Sdk } 600 'Windows SDK'
    $changed = $true
}

# 2. VC++ runtime (to run the app).
if ($Force -or -not (Test-VcRuntime)) {
    Write-Host 'Provisioning VC++ runtime...' -ForegroundColor Yellow
    $vc = Get-Installer 'https://aka.ms/vs/17/release/vc_redist.x64.exe' 'vc_redist.x64.exe'
    $p = Start-Process -FilePath $vc -ArgumentList '/install', '/quiet', '/norestart' -PassThru -Wait
    if ($p.ExitCode -ne 0) { throw "vc_redist failed (exit $($p.ExitCode))" }
    Wait-Until { Test-VcRuntime } 180 'VC++ runtime'
    $changed = $true
}

# 3. WebView2 runtime + the 64-bit registry mirror (to run the app). The base
#    image can register WebView2 in the registry while the actual runtime files
#    are absent, and/or register it only in the 32-bit view; both make the x64
#    app report "Could not find the WebView2 Runtime ... not available for this
#    one". So check the binary itself and mirror the key into the native view.
if ($Force -or -not (Test-WebView2Files)) {
    Write-Host 'Provisioning WebView2 runtime...' -ForegroundColor Yellow
    $wv = Get-Installer 'https://go.microsoft.com/fwlink/p/?LinkId=2124703' 'MicrosoftEdgeWebView2RuntimeInstallerX64.exe'
    $p = Start-Process -FilePath $wv -ArgumentList '/silent', '/install' -PassThru -Wait
    if ($p.ExitCode -ne 0) { throw "WebView2 install failed (exit $($p.ExitCode))" }
    Wait-Until { Test-WebView2Files } 300 'WebView2 runtime files'
    $changed = $true
}
if ((Test-Path $WvWow) -and -not (Test-Path $WvNative)) {
    New-Item -Path $WvNative -Force | Out-Null
    (Get-ItemProperty $WvWow).PSObject.Properties |
        Where-Object { $_.Name -notmatch '^PS' } |
        ForEach-Object { Set-ItemProperty -Path $WvNative -Name $_.Name -Value $_.Value }
    $changed = $true
}

# Verify.
$sdk = Test-Sdk
$vcr = Test-VcRuntime
$wvFiles = Test-WebView2Files
$wv2 = $wvFiles -and (Test-Path $WvNative)
Write-Host ("windows sdk : {0}" -f $sdk)
Write-Host ("vc runtime  : {0}" -f $vcr)
Write-Host ("webview2    : files={0} nativeKey={1}" -f $wvFiles, (Test-Path $WvNative))
if (-not ($sdk -and $vcr -and $wv2)) {
    throw 'toolchain is still incomplete after provisioning (see the checks above)'
}
Write-Host $(if ($changed) { 'toolchain provisioned' } else { 'toolchain already present (nothing to do)' }) -ForegroundColor Green
