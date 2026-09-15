# 实机验证：新版 exe 附加到用户正在运行的 DSH（3080），并截图确认界面
$ErrorActionPreference = 'Stop'
# 路径约定：本脚本位于 <仓库根>\tests\，$ROOT 取其父目录即仓库根，$PSScriptRoot 即本目录
$ROOT = Split-Path $PSScriptRoot -Parent
$PROJ = $ROOT
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System;
using System.Runtime.InteropServices;
public class Cap {
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
}
'@

$exe = "$PROJ\target\release\dsh-launch-console.exe"
$out = Join-Path $PSScriptRoot 'attach-shot.png'
Remove-Item "$env:TEMP\DSH-Launch-Console.log" -Force -ErrorAction SilentlyContinue

# 记录 3080 的当前拥有者（验证不会被我们重启）
$before = (Get-NetTCPConnection -LocalPort 3080 -State Listen).OwningProcess
Write-Output "3080 owner before: $before"

$p = Start-Process -FilePath $exe -PassThru
Start-Sleep -Seconds 7

$h = $p.MainWindowHandle
while ($h -eq 0 -and -not $p.HasExited) { Start-Sleep -Milliseconds 200; $p.Refresh(); $h = $p.MainWindowHandle }
Write-Output ("test instance pid={0} hwnd={1} alive={2}" -f $p.Id, $h, (-not $p.HasExited))
Write-Output "--- launcher log ---"
Get-Content "$env:TEMP\DSH-Launch-Console.log" -ErrorAction SilentlyContinue | ForEach-Object { Write-Output ("  " + $_) }

if ($h -ne 0) {
  [Cap]::ShowWindow($h, 9) | Out-Null      # SW_RESTORE
  [Cap]::SetForegroundWindow($h) | Out-Null
  Start-Sleep -Milliseconds 800
  $r = New-Object Cap+RECT
  [Cap]::GetWindowRect($h, [ref]$r) | Out-Null
  $w = $r.R - $r.L; $ht = $r.B - $r.T
  Write-Output ("window rect: {0}x{1} at {2},{3}" -f $w, $ht, $r.L, $r.T)
  $bmp = New-Object System.Drawing.Bitmap($w, $ht)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $hdc = $g.GetHdc()
  [Cap]::PrintWindow($h, $hdc, 2) | Out-Null   # PW_RENDERFULLCONTENT
  $g.ReleaseHdc($hdc)
  $g.Dispose()
  $bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
  $bmp.Dispose()
  Write-Output ("screenshot: {0} ({1} bytes)" -f $out, (Get-Item $out).Length)
}

Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 1
$after = (Get-NetTCPConnection -LocalPort 3080 -State Listen).OwningProcess
Write-Output "3080 owner after: $after  (unchanged = $($before -eq $after))"
