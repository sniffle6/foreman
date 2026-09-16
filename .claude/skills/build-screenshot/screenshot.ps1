# Launch a freshly-built foreman.exe, capture its window to win.png, then stop it.
# The GUI cannot be seen from the terminal; inspect win.png after this runs.
#
#   -Exe          exe to launch (default: .\target\debug\foreman.exe). Inside a
#                 foreman terminal build with `--target-dir target/agent` and pass
#                 .\target\agent\debug\foreman.exe so the running host is untouched.
#   -AppData      directory to use as APPDATA. config_dir() (src/config.rs) reads
#                 that variable, so the launched instance gets its own settings,
#                 themes and workspace.json and never writes over yours. Same
#                 mechanism as scripts\run-dev.ps1, whose sandbox lives at
#                 .\target\agent\appdata. Omit to use the real %APPDATA%.
#   -Out          PNG path (default: .\win.png).
#   -WaitSeconds  settle time before the capture (default: 6).
#   -Keep         leave the instance running instead of stopping it by pid.
#
# Capture is PrintWindow with PW_RENDERFULLCONTENT, not a screen grab: it reads
# the window's own surface, so a window sitting in front of foreman (a game, a
# browser) does not end up in the PNG, and nothing is pulled to the foreground.
param(
    [string] $Exe = ".\target\debug\foreman.exe",
    [string] $AppData,
    [string] $Out = ".\win.png",
    [int]    $WaitSeconds = 6,
    [switch] $Keep
)

$env:Path = "C:\w64devkit\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
if ($AppData) {
    New-Item -ItemType Directory -Force (Join-Path $AppData 'foreman') | Out-Null
    $env:APPDATA = (Resolve-Path $AppData).Path
}
# A launch from inside a foreman terminal inherits the host's identity; the new
# instance must not think it is a Session of that host.
foreach ($v in 'FOREMAN','FOREMAN_PIPE','FOREMAN_TERMINAL_ID','FOREMAN_PROJECT_ID','FOREMAN_TITLE_PIPE') {
    Remove-Item "Env:\$v" -ErrorAction SilentlyContinue
}

$p = Start-Process -FilePath $Exe -PassThru
Start-Sleep -Seconds $WaitSeconds

Add-Type @"
using System; using System.Runtime.InteropServices;
public class Cap {
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  public struct RECT { public int Left, Top, Right, Bottom; } }
"@
$p.Refresh()
for ($i = 0; $i -lt 20 -and $p.MainWindowHandle -eq 0; $i++) { Start-Sleep -Milliseconds 250; $p.Refresh() }
$h = $p.MainWindowHandle
if ($h -eq 0) { Write-Error "foreman (pid $($p.Id)) never showed a window"; exit 1 }

$r = New-Object Cap+RECT; [Cap]::GetWindowRect($h, [ref]$r) | Out-Null
Add-Type -AssemblyName System.Drawing
$b = New-Object System.Drawing.Bitmap(($r.Right - $r.Left), ($r.Bottom - $r.Top))
$g = [System.Drawing.Graphics]::FromImage($b)
$hdc = $g.GetHdc()
$ok = [Cap]::PrintWindow($h, $hdc, 2)   # 2 = PW_RENDERFULLCONTENT (GPU-composed windows)
$g.ReleaseHdc($hdc)
$png = [System.IO.Path]::GetFullPath($Out)
$b.Save($png); $g.Dispose(); $b.Dispose()
Write-Output "Saved $png ($($r.Right - $r.Left)x$($r.Bottom - $r.Top)) pid=$($p.Id) printwindow=$ok"

# Stop only the instance this script started - never by name, never by path glob.
if (-not $Keep) { Stop-Process -Id $p.Id -Force }
