# Launch the freshly-built foreman.exe, capture its window to win.png, then exit.
# The GUI can't be seen from the terminal — Read win.png after this runs.
# Source of truth: docs/HANDOFF.md section 3.
param([int]$WaitSeconds = 6)

$env:Path = "C:\w64devkit\bin;$env:USERPROFILE\.cargo\bin;$env:Path"

$p = Start-Process -FilePath ".\target\debug\foreman.exe" -PassThru
Start-Sleep -Seconds $WaitSeconds

Add-Type @"
using System; using System.Runtime.InteropServices;
public class Cap { [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  public struct RECT { public int Left, Top, Right, Bottom; } }
"@
[Cap]::SetForegroundWindow($p.MainWindowHandle) | Out-Null; Start-Sleep -Milliseconds 400
$r = New-Object Cap+RECT; [Cap]::GetWindowRect($p.MainWindowHandle, [ref]$r) | Out-Null
Add-Type -AssemblyName System.Drawing
$b = New-Object System.Drawing.Bitmap(($r.Right-$r.Left), ($r.Bottom-$r.Top))
$g = [System.Drawing.Graphics]::FromImage($b); $g.CopyFromScreen($r.Left,$r.Top,0,0,$b.Size)
$b.Save("$(Get-Location)\win.png"); $g.Dispose(); $b.Dispose()
Write-Output "Saved win.png ($($r.Right-$r.Left)x$($r.Bottom-$r.Top))"
