# 验证「窗口先显示 + 启动中页带秒数」：mock DSH 故意 10 秒后才打印 token，
# 第 5 秒截图，应看到本地“DSH 启动中…已等待 N 秒”页
$ErrorActionPreference = 'Stop'
# 路径约定：本脚本位于 <仓库根>\tests\，$ROOT 取其父目录即仓库根，$PSScriptRoot 即本目录
$ROOT = Split-Path $PSScriptRoot -Parent
$PROJ = $ROOT
$EXE  = "$PROJ\target\release\dsh-launch-console.exe"
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public class Cap2 {
  delegate bool EnumProc(IntPtr h, IntPtr p);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr p);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  // 取该进程最大的可见顶层窗口（tao 的辅助窗口很小，不能误当主窗口）
  public static IntPtr BiggestWindow(int target) {
    IntPtr best = IntPtr.Zero; long bestArea = 0;
    EnumWindows((h, p) => {
      uint pid; GetWindowThreadProcessId(h, out pid);
      if (pid == (uint)target && IsWindowVisible(h)) {
        RECT r; GetWindowRect(h, out r);
        long a = (long)(r.R - r.L) * (r.B - r.T);
        if (a > bestArea) { bestArea = a; best = h; }
      }
      return true;
    }, IntPtr.Zero);
    return best;
  }
}
'@
$T = "$env:TEMP\loading-test"
Remove-Item -Recurse -Force $T -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $T, "$T\state" | Out-Null
# 安全护栏：绝不强杀用户正在运行的 DSH Launch Console 实例（只清理实验室副本）
$live = Get-Process -Name DSH_Launch_Console -ErrorAction SilentlyContinue
if ($live) { Write-Output "!! DSH Launch Console 正在运行（pid $($live.Id -join ',')）：脚本不会动用户实例，已退出。"; exit 1 }
Get-Process -Name 'dsh-lab*','dsh-launch-console','dsh-browser' -ErrorAction SilentlyContinue | Stop-Process -Force
Get-NetTCPConnection -LocalPort 3199 -State Listen -ErrorAction SilentlyContinue |
  ForEach-Object { Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue }
Start-Sleep -Milliseconds 600
@'
const http = require('http');
const token = 'SLOWTOKEN123456';
setTimeout(() => console.log('dsh web: http://127.0.0.1:3199/?token=' + token), 10000);
http.createServer((q, s) => {
  const ok = q.url.indexOf('token=' + token) >= 0;
  if (ok) { s.writeHead(200, {'Content-Type':'text/html'}); s.end('<title>MOCK-DSH</title>real ui'); }
  else { s.writeHead(401); s.end('unauthorized'); }
}).listen(3199);
'@ | Set-Content -Encoding ASCII "$T\mock.js"
@"
@echo off
node "$T\mock.js"
"@ | Set-Content -Encoding ASCII "$T\dsh.cmd"

$env:DSH_LAUNCH_CONSOLE_URL = 'http://127.0.0.1:3199'
$env:DSH_LAUNCH_CONSOLE_STATEDIR = "$T\state"
$env:DSH_LAUNCH_CONSOLE_NOMSGBOX = '1'
$env:PATH = "$T;C:\Program Files\nodejs;C:\Windows\system32;C:\Windows"
Remove-Item "$env:TEMP\DSH-Launch-Console.log" -Force -ErrorAction SilentlyContinue

$p = Start-Process -FilePath $EXE -PassThru
$sw = [System.Diagnostics.Stopwatch]::StartNew()
$h = [IntPtr]::Zero
while ($sw.Elapsed.TotalSeconds -lt 3 -and $h -eq [IntPtr]::Zero) {
  Start-Sleep -Milliseconds 50
  $h = [Cap2]::BiggestWindow($p.Id)
}
Write-Output ("主窗口出现: {0}ms (hwnd={1})" -f [int]$sw.Elapsed.TotalMilliseconds, $h)
# 等到约 5.5 秒（token 还没出现）截图
while ($sw.Elapsed.TotalSeconds -lt 5.5) { Start-Sleep -Milliseconds 100 }
$h = [Cap2]::BiggestWindow($p.Id)
[Cap2]::ShowWindow($h, 9) | Out-Null
[Cap2]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Milliseconds 500
$r = New-Object Cap2+RECT
[Cap2]::GetWindowRect($h, [ref]$r) | Out-Null
$w = $r.R - $r.L; $ht = $r.B - $r.T
$bmp = New-Object System.Drawing.Bitmap($w, $ht)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$dc = $g.GetHdc(); [Cap2]::PrintWindow($h, $dc, 2) | Out-Null; $g.ReleaseHdc($dc); $g.Dispose()
$out = Join-Path $PSScriptRoot 'loading-shot.png'
$bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose()
Write-Output ("截图: {0} ({1} bytes, {2}x{3})" -f $out, (Get-Item $out).Length, $w, $ht)
Write-Output "--- 启动器日志 ---"
Get-Content "$env:TEMP\DSH-Launch-Console.log" -ErrorAction SilentlyContinue | ForEach-Object { Write-Output ("  " + $_) }
Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 1


