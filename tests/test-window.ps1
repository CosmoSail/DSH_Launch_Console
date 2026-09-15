# 窗口可见时间测量：进程启动 → 出现第一个可见顶层窗口的毫秒数
$ErrorActionPreference = 'Stop'
# 路径约定：本脚本位于 <仓库根>\tests\，$ROOT 取其父目录即仓库根，$PSScriptRoot 即本目录
$ROOT = Split-Path $PSScriptRoot -Parent
$PROJ = $ROOT
Add-Type @'
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public class Win {
  delegate bool EnumProc(IntPtr h, IntPtr p);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr p);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
  public static List<string> VisibleWindows(int target) {
    var res = new List<string>();
    EnumWindows((h, p) => {
      uint pid; GetWindowThreadProcessId(h, out pid);
      if (pid == (uint)target && IsWindowVisible(h)) {
        var sb = new StringBuilder(256); GetWindowTextW(h, sb, 256);
        res.Add(h.ToString() + "|" + sb.ToString());
      }
      return true;
    }, IntPtr.Zero);
    return res;
  }
}
'@

$OLD = "$ROOT\DSH_Launch_Console.exe"
$NEW = "$PROJ\target\release\dsh-launch-console.exe"
$NODE_DIR = 'C:\Program Files\nodejs'
$SYS = "C:\Windows\system32;C:\Windows"
$mockJs = @'
const http = require('http'), fs = require('fs');
const dir = process.env.MOCK_DIR, token = process.env.MOCK_TOKEN;
setTimeout(() => console.log('dsh web: http://127.0.0.1:3199/?token=' + token), 150);
http.createServer((q, s) => {
  const ok = q.url.indexOf('token=' + token) >= 0;
  if (ok) { s.writeHead(200, {'Content-Type':'text/html'}); s.end('<title>MOCK-DSH</title>mock'); }
  else { s.writeHead(401); s.end('unauthorized'); }
}).listen(3199);
'@

function Measure-Window {
  param([string]$Name, [string]$Exe, [string]$Kind, [int]$DelaySec)
  $T = Join-Path $env:TEMP ("win-" + $Name)
  Remove-Item -Recurse -Force $T -ErrorAction SilentlyContinue
  New-Item -ItemType Directory -Force -Path $T, "$T\state" | Out-Null
  # 安全护栏：绝不强杀用户正在运行的 DSH Launch Console 实例（只清理实验室副本）
$live = Get-Process -Name DSH_Launch_Console -ErrorAction SilentlyContinue
if ($live) { Write-Output "!! DSH Launch Console 正在运行（pid $($live.Id -join ',')）：脚本不会动用户实例，已退出。"; exit 1 }
Get-Process -Name 'dsh-lab*','dsh-launch-console','dsh-browser' -ErrorAction SilentlyContinue | Stop-Process -Force
  Get-NetTCPConnection -LocalPort 3199 -State Listen -ErrorAction SilentlyContinue |
    ForEach-Object { Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue }
  Start-Sleep -Milliseconds 400
  Set-Content -Encoding ASCII (Join-Path $T 'mock.js') $mockJs
  $delay = if ($DelaySec -gt 0) { "ping -n $($DelaySec + 1) 127.0.0.1 >nul`r`n" } else { '' }
  Set-Content -Encoding ASCII (Join-Path $T "$Kind.cmd") "@echo off`r`n${delay}node `"$T\mock.js`""

  $env:DSH_LAUNCH_CONSOLE_URL = 'http://127.0.0.1:3199'
  $env:DSH_LAUNCH_CONSOLE_STATEDIR = "$T\state"
  $env:DSH_LAUNCH_CONSOLE_NOMSGBOX = '1'
  $env:MOCK_DIR = $T
  $env:MOCK_TOKEN = 'TESTTOKEN123456'
  $env:PATH = "$T;$NODE_DIR;$SYS"

  $p = Start-Process -FilePath $Exe -PassThru
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  $found = $null
  while ($sw.Elapsed.TotalSeconds -lt 30) {
    $w = [Win]::VisibleWindows($p.Id)
    if ($w.Count -gt 0) { $found = $w[0]; break }
    Start-Sleep -Milliseconds 15
  }
  if ($found) { Write-Output ("{0,-22} 窗口可见: {1,5}ms  [{2}]" -f $Name, [int]$sw.Elapsed.TotalMilliseconds, ($found -split '\|')[1]) }
  else { Write-Output ("{0,-22} !! 30 秒内没有可见窗口 (alive={1})" -f $Name, (-not $p.HasExited)) }
  Write-Output "  --- launcher log ---"
  Get-Content (Join-Path $env:TEMP 'DSH-Launch-Console.log') -ErrorAction SilentlyContinue |
    ForEach-Object { Write-Output ("  " + $_) }
  Write-Output "  --- server log ---"
  Get-Content (Join-Path $env:TEMP 'DSH-Launch-Console-server.log') -ErrorAction SilentlyContinue | Select-Object -First 6 |
    ForEach-Object { Write-Output ("  " + $_) }
  Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
  Start-Sleep -Milliseconds 900
}

Measure-Window -Name 'A-old-npx-2s' -Exe $OLD -Kind 'npx' -DelaySec 2
Measure-Window -Name 'A2-old-npx-0s' -Exe $OLD -Kind 'npx' -DelaySec 0
Measure-Window -Name 'B-new-dsh' -Exe $NEW -Kind 'dsh' -DelaySec 0
Measure-Window -Name 'B2-new-dsh' -Exe $NEW -Kind 'dsh' -DelaySec 0
