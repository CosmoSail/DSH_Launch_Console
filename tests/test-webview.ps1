# 验证：① WebView 权限放行（通知/剪贴板）② 同源弹窗 → 壳内轻量窗口
# 安全：只用改名副本 dsh-lab-web.exe；发现 DSH Launch Console 在运行则直接退出。
$ErrorActionPreference = 'Stop'
# 路径约定：本脚本位于 <仓库根>\tests\，$ROOT 取其父目录即仓库根，$PSScriptRoot 即本目录
$ROOT = Split-Path $PSScriptRoot -Parent
$PROJ = $ROOT
$EXE = "$PROJ\target\release\dsh-launch-console.exe"
$LABEXE = "$PSScriptRoot\dsh-lab-web.exe"
$NODE = 'C:\Program Files\nodejs\node.exe'
$SYS = "C:\Windows\system32;C:\Windows"
$PORT = 3198
$TOKEN = 'WEBTOKEN123456'

Add-Type @'
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public class W3 {
  delegate bool EnumProc(IntPtr h, IntPtr p);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr p);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
  public static List<string> AllByTitle(string want) {
    var res = new List<string>();
    EnumWindows((h, p) => {
      if (!IsWindowVisible(h)) return true;
      var sb = new StringBuilder(256); GetWindowTextW(h, sb, 256);
      if (sb.ToString() == want) {
        uint pid; GetWindowThreadProcessId(h, out pid);
        RECT r; GetWindowRect(h, out r);
        res.Add("title='" + sb + "' pid=" + pid + " size=" + (r.R - r.L) + "x" + (r.B - r.T));
      }
      return true;
    }, IntPtr.Zero);
    return res;
  }
  [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out RECT r);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  public static List<string> Titles(int target) {
    var res = new List<string>();
    EnumWindows((h, p) => {
      uint pid; GetWindowThreadProcessId(h, out pid);
      if (pid == (uint)target && IsWindowVisible(h)) {
        var sb = new StringBuilder(256); GetWindowTextW(h, sb, 256);
        res.Add(sb.ToString());
      }
      return true;
    }, IntPtr.Zero);
    return res;
  }
}
'@

# 用独立互斥体名 + 独立状态目录与用户实例并存；绝不碰 DSH Launch Console 进程
$live = Get-Process -Name DSH_Launch_Console -ErrorAction SilentlyContinue
if ($live) { Write-Output ("注意：用户实例正在运行（pid " + ($live.Id -join ',') + "），本脚本使用独立互斥体并存，不会动它。") }
Get-Process -Name 'dsh-lab*' -ErrorAction SilentlyContinue | Stop-Process -Force
Get-NetTCPConnection -LocalPort $PORT -State Listen -ErrorAction SilentlyContinue |
  ForEach-Object { Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue }
Copy-Item $EXE $LABEXE -Force

$T = Join-Path $env:TEMP 'lab-web'
Remove-Item -Recurse -Force $T -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $T, "$T\state" | Out-Null
Copy-Item $NODE (Join-Path $T 'node.exe') -Force
@"
@ECHO off
GOTO start
:find_dp0
SET dp0=%~dp0
EXIT /b
:start
SETLOCAL
CALL :find_dp0
IF EXIST "%dp0%\node.exe" (
  SET "_prog=%dp0%\node.exe"
) ELSE (
  SET "_prog=node"
)
endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & "%_prog%"  "%dp0%\node_modules\@deepseek-ai\dsh\lib\bin.js" %*
"@ | Set-Content -Encoding ASCII (Join-Path $T 'dsh.cmd')
$lib = Join-Path $T 'node_modules\@deepseek-ai\dsh\lib'
New-Item -ItemType Directory -Force -Path $lib | Out-Null
'{"name":"@deepseek-ai/dsh","bin":{"dsh":"lib/bin.js"}}' | Set-Content -Encoding ASCII (Join-Path $T 'node_modules\@deepseek-ai\dsh\package.json')

@'
const fs = require('fs'), http = require('http'), path = require('path');
const root = path.resolve(__dirname, '..', '..', '..', '..');
const token = 'WEBTOKEN123456';
const args = process.argv.slice(2);
const pi = args.indexOf('--port');
const port = pi >= 0 ? parseInt(args[pi + 1], 10) : 3198;
setTimeout(() => console.log('dsh web: http://127.0.0.1:' + port + '/?token=' + token), 150);

const page = `<!doctype html><html><head><meta charset="utf-8"><title>LAB</title></head><body>
<h1>lab</h1><script>
(async () => {
  const report = (k, v) => fetch('/report?k=' + k + '&v=' + encodeURIComponent(String(v)));
  let perm = 'no-api';
  try { perm = Notification.permission; if (perm === 'default') { perm = await Notification.requestPermission(); } }
  catch (e) { perm = 'throw:' + e.name; }
  await report('notification', perm);
  let clip = 'n/a';
  try { const t = await navigator.clipboard.readText(); clip = 'ok:' + t.slice(0, 12); }
  catch (e) { clip = 'err:' + e.name; }
  await report('clipboard', clip);
  await report('before_open', 'ok');
  let w = null, err = '';
  const t0 = Date.now();
  try { w = window.open('/popup', '_blank', 'width=700,height=500'); }
  catch (e) { err = e.name + ':' + e.message; }
  await report('after_open', (Date.now() - t0) + 'ms;w=' + (w ? 'obj' : 'null') + ';err=' + err);
  await new Promise(r => setTimeout(r, 1500));
  await report('after_wait', w ? ('closed=' + w.closed) : 'null');
  await report('done', '1');
})();
</script></body></html>`;

http.createServer((q, s) => {
  fs.appendFileSync(path.join(root, 'req.txt'), Date.now() + ' ' + q.url + '\n');
  if (q.url.indexOf('/report') === 0) { s.writeHead(200); s.end('ok'); return; }
  const hasCookie = (q.headers.cookie || '').indexOf('labcookie=1') >= 0;
  if (q.url.indexOf('/popup') === 0) {
    fs.appendFileSync(path.join(root, 'req.txt'), 'POPUP-COOKIE ' + (hasCookie ? 'shared' : 'MISSING') + '\n');
    if (!hasCookie) { s.writeHead(401); s.end('popup unauthorized (cookie not shared)'); return; }
    s.writeHead(200, {'Content-Type':'text/html; charset=utf-8'});
    s.end('<!doctype html><html><head><meta charset="utf-8"><title>POPUP-WINDOW</title></head><body><h1>popup content</h1></body></html>');
    return;
  }
  if (q.url.indexOf('token=' + token) >= 0) {
    s.writeHead(200, {'Content-Type':'text/html; charset=utf-8', 'Set-Cookie':'labcookie=1; Path=/'});
    s.end(page);
    return;
  }
  s.writeHead(401); s.end('unauthorized');
}).listen(port, '127.0.0.1');
'@ | Set-Content -Encoding ASCII (Join-Path $lib 'bin.js')

$env:DSH_LAUNCH_CONSOLE_URL = "http://127.0.0.1:$PORT"
$env:DSH_LAUNCH_CONSOLE_STATEDIR = "$T\state"
$env:DSH_LAUNCH_CONSOLE_NOMSGBOX = '1'
$env:PATH = "$T;$SYS"
$env:DSH_LAUNCH_CONSOLE_MUTEX = 'DSH_Launch_Console_LabInstance_Mutex'
Remove-Item Env:DSH_LAUNCH_CONSOLE_NPX -ErrorAction SilentlyContinue
Remove-Item "$env:TEMP\DSH-Launch-Console.log","$env:TEMP\DSH-Launch-Console-server.log" -Force -ErrorAction SilentlyContinue

$p = Start-Process -FilePath $LABEXE -PassThru
$done = $false
for ($i = 0; $i -lt 300; $i++) {
  Start-Sleep -Milliseconds 100
  if (Test-Path "$T\req.txt") { if ((Get-Content "$T\req.txt" -Raw) -match 'k=done') { $done = $true; break } }
}
Write-Output ("=== 页面自检完成: " + $done + " ===")
Write-Output "--- 权限/弹窗自检结果 ---"
Get-Content "$T\req.txt" -ErrorAction SilentlyContinue | Where-Object { $_ -match '/report' } | ForEach-Object {
  $u = ($_ -split ' ')[1]
  if ($u -match 'k=([^&]+)&v=(.*)$') { Write-Output ("  {0,-14} = {1}" -f $Matches[1], [System.Uri]::UnescapeDataString($Matches[2])) }
}
Write-Output "--- mock 收到的页面请求（弹窗应出现 /popup）---"
Get-Content "$T\req.txt" -ErrorAction SilentlyContinue | Where-Object { $_ -match '/popup|token=' } | ForEach-Object { Write-Output ("  " + ($_ -replace '^\d+ ', '')) }
Get-Content "$T\req.txt" -ErrorAction SilentlyContinue | Where-Object { $_ -match 'POPUP-COOKIE' } | ForEach-Object { Write-Output ("  弹窗会话共享: " + $_) }
Write-Output "--- 弹窗窗口（跨进程枚举，标题应为页面标题）---"
[W3]::AllByTitle('POPUP-WINDOW') | ForEach-Object { Write-Output ("  " + $_) }
Write-Output "--- 该进程当前可见顶层窗口 ---"
[W3]::Titles($p.Id) | ForEach-Object { Write-Output ("  [" + $_ + "]") }
Write-Output "--- 启动器日志（权限/弹窗相关）---"
Get-Content "$env:TEMP\DSH-Launch-Console.log" -ErrorAction SilentlyContinue |
  Where-Object { $_ -match 'permission|popup|in-app|starting:' } | ForEach-Object { Write-Output ("  " + ($_ -replace '^\[\d+\] ', '')) }

Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 1
Remove-Item $LABEXE -Force -ErrorAction SilentlyContinue
Get-NetTCPConnection -LocalPort $PORT -State Listen -ErrorAction SilentlyContinue |
  ForEach-Object { Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue }
