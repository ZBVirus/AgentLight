[CmdletBinding()]
param(
    [string]$Exe = "",
    [string]$OutDir = "",
    [int]$WaitMs = 20000
)

$ErrorActionPreference = "Continue"
$scriptDir = if ($PSScriptRoot) { $PSScriptRoot } else { (Get-Location).Path }
if (-not $OutDir) { $OutDir = Join-Path $scriptDir "ui-report" }
if (-not $Exe) {
    foreach ($candidate in @(
        (Join-Path $scriptDir "..\target\ci\agentlight.exe"),
        (Join-Path $scriptDir "..\target\release\agentlight.exe"),
        (Join-Path $scriptDir "AgentLight-portable.exe"),
        "C:\SandboxOutput\build\AgentLight-portable.exe"
    )) { if (Test-Path $candidate) { $Exe = (Resolve-Path $candidate).Path; break } }
}
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$script:Report = New-Object System.Collections.Generic.List[string]
function Log($msg) { $script:Report.Add([string]$msg); Write-Host $msg }

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
try {
    Add-Type -AssemblyName UIAutomationClient
    Add-Type -AssemblyName UIAutomationTypes
    $script:HasUia = $true
} catch {
    $script:HasUia = $false
}

$csharp = @'
using System;
using System.Text;
using System.Threading;
using System.Runtime.InteropServices;
public static class ALWin {
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr hWnd, StringBuilder s, int n);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT r);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hWnd, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] public static extern int GetWindowLong(IntPtr hWnd, int i);
    [DllImport("user32.dll")] public static extern IntPtr GetWindow(IntPtr hWnd, uint cmd);
    [DllImport("user32.dll")] public static extern IntPtr GetTopWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint d, UIntPtr e);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr hWnd, int x, int y, int w, int h, bool p);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
    const uint MOUSEEVENTF_LEFTDOWN = 0x0002, MOUSEEVENTF_LEFTUP = 0x0004;

    public static IntPtr FindMainWindow(uint pid) {
        IntPtr best = IntPtr.Zero; long bestArea = 0;
        EnumWindows(delegate(IntPtr h, IntPtr l) {
            if (!IsWindowVisible(h)) return true;
            uint p; GetWindowThreadProcessId(h, out p);
            if (p != pid) return true;
            RECT r; GetWindowRect(h, out r);
            long area = (long)(r.Right - r.Left) * (r.Bottom - r.Top);
            if (area > bestArea) { bestArea = area; best = h; }
            return true;
        }, IntPtr.Zero);
        return best;
    }
    public static int Style(IntPtr h) { return GetWindowLong(h, -16); }
    public static int ExStyle(IntPtr h) { return GetWindowLong(h, -20); }
    public static bool IsResizable(IntPtr h) { return (Style(h) & 0x00040000) != 0; }
    public static bool IsTopmost(IntPtr h) { return (ExStyle(h) & 0x00000008) != 0; }
    public static string Text(IntPtr h) { StringBuilder s = new StringBuilder(512); GetWindowText(h, s, 512); return s.ToString(); }
    public static int[] Rect(IntPtr h) { RECT r; GetWindowRect(h, out r); return new int[] { r.Left, r.Top, r.Right - r.Left, r.Bottom - r.Top }; }
    public static int ZRank(IntPtr h) {
        int i = 0; IntPtr w = GetTopWindow(IntPtr.Zero);
        while (w != IntPtr.Zero) { if (w == h) return i; i++; w = GetWindow(w, 2); }
        return -1;
    }
    public static void Click(int x, int y) {
        SetCursorPos(x, y); Thread.Sleep(150);
        mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, UIntPtr.Zero); Thread.Sleep(90);
        mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, UIntPtr.Zero); Thread.Sleep(150);
    }
    public static void Drag(int x1, int y1, int x2, int y2) {
        SetCursorPos(x1, y1); Thread.Sleep(200);
        mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, UIntPtr.Zero); Thread.Sleep(120);
        int steps = 14;
        for (int i = 1; i <= steps; i++) {
            SetCursorPos(x1 + (x2 - x1) * i / steps, y1 + (y2 - y1) * i / steps);
            Thread.Sleep(25);
        }
        mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, UIntPtr.Zero); Thread.Sleep(200);
    }
    public static void Move(IntPtr h, int x, int y, int w, int hh) { MoveWindow(h, x, y, w, hh, true); }
    public static bool Foreground(IntPtr h) { return SetForegroundWindow(h); }
}
'@
Add-Type -TypeDefinition $csharp

function Save-Screenshot([string]$Path, [IntPtr]$Handle = [IntPtr]::Zero) {
    # In a headless/interactive-but-no-console session, CopyFromScreen throws
    # "The handle is invalid"; PrintWindow still renders the window's own
    # content without a visible desktop. Prefer PrintWindow when we have the
    # window handle, and fall back to the virtual screen otherwise.
    try {
        if ($Handle -ne [IntPtr]::Zero) {
            $r = [ALWin]::Rect($Handle)
            if ($r[2] -gt 0 -and $r[3] -gt 0) {
                $bmp = New-Object System.Drawing.Bitmap($r[2], $r[3])
                $g = [System.Drawing.Graphics]::FromImage($bmp)
                $hdc = $g.GetHdc()
                $ok = [ALWin]::PrintWindow($Handle, $hdc, 0)
                $g.ReleaseHdc($hdc); $g.Dispose()
                $bmp.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
                $bmp.Dispose()
                Log ("screenshot (PrintWindow ok=$ok): $Path")
                return
            }
        }
        $b = [System.Windows.Forms.SystemInformation]::VirtualScreen
        $bmp = New-Object System.Drawing.Bitmap($b.Width, $b.Height)
        $g = [System.Drawing.Graphics]::FromImage($bmp)
        $g.CopyFromScreen($b.X, $b.Y, 0, 0, $bmp.Size)
        $bmp.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
        $g.Dispose(); $bmp.Dispose()
        Log ("screenshot: $Path")
    } catch { Log ("screenshot failed: " + $_.Exception.Message) }
}

function Invoke-UiaByName([string]$Name) {
    if (-not $script:HasUia) { return $false }
    try {
        $root = [System.Windows.Automation.AutomationElement]::RootElement
        $cond = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, $Name)
        $el = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $cond)
        if ($null -eq $el) { return $false }
        $pattern = $null
        if ($el.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)) {
            $pattern.Invoke(); return $true
        }
    } catch { Log ("UIA '$Name' failed: " + $_.Exception.Message) }
    return $false
}

function Get-ChildConsole([int]$ParentId) {
    $bad = @()
    Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | Where-Object { $_.ParentProcessId -eq $ParentId } | ForEach-Object {
        if ($_.Name -match '^(cmd|conhost|powershell|pwsh|wscript|cscript)\.exe$') { $bad += ($_.Name + "#" + $_.ProcessId) }
    }
    return $bad
}

if (-not (Test-Path $Exe)) {
    Log ("FATAL: executable not found: " + $Exe)
    Log "Pass -Exe <path> to point at the AgentLight build."
    $script:Report | Set-Content -Path (Join-Path $OutDir "report.txt") -Encoding UTF8
    return
}

Get-Process -Name 'AgentLight*' -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Get-Process notepad -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 1

$configPath = Join-Path $env:APPDATA "AgentLight\config.json"
$original = $null
if (Test-Path $configPath) { try { $original = Get-Content $configPath -Raw | ConvertFrom-Json } catch {} }
$style = if ($original) { $original.collapse_style } else { "single" }
$sizeMap = @{ single = @(88,88); triple = @(172,68); triple_vertical = @(68,172) }
$expected = $sizeMap[$style]
if (-not $expected) { $expected = @(88,88) }

Log "AgentLight UI harness"
Log ("date=" + (Get-Date).ToString("s"))
Log ("SESSIONNAME=" + $env:SESSIONNAME + "  user=" + $env:USERNAME)
Log ("monitors=" + ([System.Windows.Forms.Screen]::AllScreens.Count))
Log ("exe=" + $Exe)
Log ("collapse_style=$style expected_collapsed=$($expected[0])x$($expected[1])")

$app = $null
$hwnd = [IntPtr]::Zero
try {
    $app = Start-Process -FilePath $Exe -PassThru
    $deadline = (Get-Date).AddMilliseconds($WaitMs)
    while ((Get-Date) -lt $deadline -and $hwnd -eq [IntPtr]::Zero) {
        Start-Sleep -Milliseconds 300
        $hwnd = [ALWin]::FindMainWindow($app.Id)
    }
    Log ""
    Log "== launch =="
    if ($hwnd -eq [IntPtr]::Zero) {
        Log "FAIL: no visible window appeared (not interactive, or the app did not start)"
        Log ("process alive=" + (-not $app.HasExited))
    } else {
        Log ("main hwnd=" + $hwnd + " title='" + [ALWin]::Text($hwnd) + "'")
    }

    if ($hwnd -ne [IntPtr]::Zero) {
        # Wait for the frontend to load and finish its initial resize; the
        # WebView2 window exists before the JS wires events and sizes the view.
        $prev = ''
        $stable = 0
        for ($i = 0; $i -lt 25; $i++) {
            Start-Sleep -Milliseconds 400
            $cur = ([ALWin]::Rect($hwnd) -join ',')
            if ($cur -eq $prev) { $stable++; if ($stable -ge 2) { break } } else { $stable = 0 }
            $prev = $cur
        }
        Log ("settled rect=" + ([ALWin]::Rect($hwnd) -join ","))

        $r = [ALWin]::Rect($hwnd)
        Log ("collapsed rect=" + ($r -join ","))
        Save-Screenshot (Join-Path $OutDir "01-collapsed.png") $hwnd
        Log ("resizable(WS_THICKFRAME)=" + [ALWin]::IsResizable($hwnd) + "   (expected False)")
        Log ("topmost(WS_EX_TOPMOST)=" + [ALWin]::IsTopmost($hwnd))
        Log ("size matches expected: " + ($r[2] -eq $expected[0] -and $r[3] -eq $expected[1]))

        $beforeR = [ALWin]::Rect($hwnd)
        [ALWin]::Foreground($hwnd) | Out-Null
        Start-Sleep -Milliseconds 300
        [ALWin]::Drag(($beforeR[0] + $beforeR[2] - 3), ($beforeR[1] + $beforeR[3] - 3), ($beforeR[0] + $beforeR[2] + 60), ($beforeR[1] + $beforeR[3] + 60))
        Start-Sleep -Milliseconds 400
        $afterR = [ALWin]::Rect($hwnd)
        Log ("resize attempt: before=" + ($beforeR -join ",") + " after=" + ($afterR -join ",") + " changed=" + (($beforeR -join ",") -ne ($afterR -join ",")) + " (expected changed=False)")

        [ALWin]::Foreground($hwnd) | Out-Null
        Start-Sleep -Milliseconds 300
        $r = [ALWin]::Rect($hwnd)
        [ALWin]::Click(($r[0] + [int]($r[2]/2)), ($r[1] + [int]($r[3]/2)))
        Start-Sleep -Milliseconds 1200
        $r2 = [ALWin]::Rect($hwnd)
        Log ""
        Log "== expand =="
        Log ("detail rect=" + ($r2 -join ",") + " (expected detail 420x548)")
        Save-Screenshot (Join-Path $OutDir "02-detail.png") $hwnd

        Log ""
        Log "== pin / topmost =="
        $found = Invoke-UiaByName "Toggle always on top"
        Log ("found pin button: " + $found)
        if ($found) {
            Start-Sleep -Milliseconds 800
            Log ("after first toggle: topmost=" + [ALWin]::IsTopmost($hwnd))
            Invoke-UiaByName "Toggle always on top" | Out-Null
            Start-Sleep -Milliseconds 800
            Log ("after second toggle: topmost=" + [ALWin]::IsTopmost($hwnd))
            if (-not [ALWin]::IsTopmost($hwnd)) { Invoke-UiaByName "Toggle always on top" | Out-Null; Start-Sleep -Milliseconds 800 }
        }
        Log ("pin on -> topmost=" + [ALWin]::IsTopmost($hwnd))

        Log ""
        Log "== z-order vs Notepad =="
        $np = Start-Process notepad -PassThru
        Start-Sleep -Milliseconds 1500
        $nh = [ALWin]::FindMainWindow($np.Id)
        if ($nh -ne [IntPtr]::Zero) {
            [ALWin]::Move($nh, 150, 150, 500, 400)
            [ALWin]::Foreground($nh) | Out-Null
            Start-Sleep -Milliseconds 500
            Log ("pin on: app z=" + [ALWin]::ZRank($hwnd) + " notepad z=" + [ALWin]::ZRank($nh) + " (smaller = higher; app should be above)")
            Save-Screenshot (Join-Path $OutDir "03-topmost-on.png") $hwnd
        } else { Log "notepad window not found" }
        Invoke-UiaByName "Toggle always on top" | Out-Null
        Start-Sleep -Milliseconds 800
        if ($nh -ne [IntPtr]::Zero) {
            [ALWin]::Foreground($nh) | Out-Null
            Start-Sleep -Milliseconds 500
            Log ("pin off: topmost=" + [ALWin]::IsTopmost($hwnd) + " app z=" + [ALWin]::ZRank($hwnd) + " notepad z=" + [ALWin]::ZRank($nh))
        }
        Stop-Process -Id $np.Id -Force -ErrorAction SilentlyContinue

        Log ""
        Log "== collapse =="
        $c = Invoke-UiaByName "Collapse"
        Log ("found Collapse button: " + $c)
        if ($c) { Start-Sleep -Milliseconds 1200 } else {
            $r = [ALWin]::Rect($hwnd)
            [ALWin]::Click(($r[0] + $r[2] - 60), ($r[1] + 16))
            Start-Sleep -Milliseconds 1200
        }
        $r3 = [ALWin]::Rect($hwnd)
        Log ("re-collapsed rect=" + ($r3 -join ",") + " (expected " + $expected[0] + "x" + $expected[1] + ")")
        Save-Screenshot (Join-Path $OutDir "04-recollapsed.png") $hwnd

        Log ""
        Log "== same-monitor =="
        $screens = [System.Windows.Forms.Screen]::AllScreens
        if ($screens.Count -lt 2) {
            Log "only one monitor; skipped"
        } else {
            $s1 = $screens[1].Bounds
            $tx = $s1.X + 40; $ty = $s1.Y + 40
            [ALWin]::Move($hwnd, $tx, $ty, $r3[2], $r3[3])
            Start-Sleep -Milliseconds 600
            $monBefore = [System.Windows.Forms.Screen]::FromHandle($hwnd).DeviceName
            [ALWin]::Click(($tx + [int]($r3[2]/2)), ($ty + [int]($r3[3]/2)))
            Start-Sleep -Milliseconds 1200
            $monAfter = [System.Windows.Forms.Screen]::FromHandle($hwnd).DeviceName
            Log ("monitor before=$monBefore after=$monAfter same=" + ($monBefore -eq $monAfter))
            Invoke-UiaByName "Collapse" | Out-Null
            Start-Sleep -Milliseconds 1000
        }

        Log ""
        Log "== console-spawn check =="
        $bad = Get-ChildConsole $app.Id
        Log ("console-like children after launch+wnd ops: " + $(if ($bad.Count) { $bad -join "," } else { "none" }))
    }
} catch {
    Log ("UNCAUGHT: " + $_.Exception.Message)
} finally {
    Log ""
    Log "== notification identity =="
    $key = "HKCU:\Software\Classes\AppUserModelId\com.zbvirus.agentlight"
    if (Test-Path $key) {
        $p = Get-ItemProperty $key
        Log ("AUMID DisplayName=" + $p.DisplayName + " IconUri=" + $p.IconUri)
    } else { Log "AUMID key MISSING" }

    Get-Process -Name 'AgentLight*' -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Get-Process notepad -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue

    if ($original) {
        try {
            $original | ConvertTo-Json -Depth 8 | Set-Content -Path $configPath -Encoding UTF8
            Log "restored config.json"
        } catch { Log ("could not restore config: " + $_.Exception.Message) }
    }

    $script:Report | Set-Content -Path (Join-Path $OutDir "report.txt") -Encoding UTF8
    Write-Host ("report: " + (Join-Path $OutDir "report.txt"))
}
