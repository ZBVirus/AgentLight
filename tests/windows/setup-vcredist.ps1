$ErrorActionPreference = 'Continue'
$out = 'C:\SandboxOutput\vc_redist.x64.exe'
New-Item -ItemType Directory -Force -Path 'C:\SandboxOutput' | Out-Null

Write-Output '== download =='
try {
    Invoke-WebRequest -Uri 'https://aka.ms/vs/17/release/vc_redist.x64.exe' -OutFile $out -UseBasicParsing
    Write-Output ("downloaded: {0} bytes" -f (Get-Item $out).Length)
} catch {
    Write-Output ("download failed: {0}" -f $_.Exception.Message)
    exit 1
}

Write-Output '== install =='
$p = Start-Process -FilePath $out -ArgumentList '/install', '/quiet', '/norestart' -PassThru -Wait
Write-Output ("installer exit code: {0}" -f $p.ExitCode)

Write-Output '== runtime present? =='
Get-ChildItem 'C:\Windows\System32\vcruntime140.dll', 'C:\Windows\System32\vcruntime140_1.dll', 'C:\Windows\System32\msvcp140.dll' -ErrorAction SilentlyContinue |
    Select-Object Name, Length | Format-Table -AutoSize
