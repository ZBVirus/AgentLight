$ErrorActionPreference = 'Continue'
Start-Transcript -Path 'C:\SandboxOutput\fix-webview.txt' -Force | Out-Null
$guid = '{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'

'== EdgeUpdate keys =='
foreach ($view in @('HKLM:\SOFTWARE\Microsoft', 'HKLM:\SOFTWARE\WOW6432Node\Microsoft', 'HKCU:\Software\Microsoft')) {
    $k = Join-Path $view "EdgeUpdate\Clients\$guid"
    if (Test-Path $k) { "OK $k"; (Get-ItemProperty $k | Format-List | Out-String) } else { "missing $k" }
}

'== PE arch of msedgewebview2.exe =='
$exe = 'C:\Program Files (x86)\Microsoft\EdgeWebView\Application\155.0.4283.45\msedgewebview2.exe'
if (Test-Path $exe) {
    $fs = [IO.File]::OpenRead($exe); $br = New-Object IO.BinaryReader($fs)
    $fs.Seek(0x3C, 'Begin') | Out-Null; $off = $br.ReadInt32()
    $fs.Seek($off + 4, 'Begin') | Out-Null; $m = $br.ReadUInt16(); $fs.Close()
    "machine=0x{0:X}  (8664=x64, 14C=x86)" -f $m
} else { "exe not found" }

'== mirror key into native (64-bit) view =='
$src = "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\$guid"
$dst = "HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\$guid"
if (Test-Path $src) {
    New-Item -Path $dst -Force | Out-Null
    (Get-ItemProperty $src).PSObject.Properties |
        Where-Object { $_.Name -notmatch '^PS' } |
        ForEach-Object { Set-ItemProperty -Path $dst -Name $_.Name -Value $_.Value }
    'copied; native view now:'
    (Get-ItemProperty $dst | Format-List | Out-String)
} else { 'source missing' }

Stop-Transcript | Out-Null
