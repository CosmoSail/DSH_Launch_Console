# 实测我方壳（wry/WebView2）对通知权限的默认处理：
# mock 页面调用 Notification.requestPermission()，把结果回传到 mock 服务器日志
$ErrorActionPreference = 'Stop'
# 路径约定：本脚本位于 <仓库根>\tests\，$ROOT 取其父目录即仓库根，$PSScriptRoot 即本目录
$ROOT = Split-Path $PSScriptRoot -Parent
$PROJ = $ROOT
$T = "$env:TEMP\perm-test"
Remove-Item -Recurse -Force $T -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $T, "$T\state" | Out-Null
# 安全护栏：绝不强杀用户正在运行的 DSH Launch Console 实例（只清理实验室副本）
$live = Get-Process -Name DSH_Launch_Console -ErrorAction SilentlyContinue
if ($live) { Write-Output "!! DSH Launch Console 正在运行（pid $($live.Id -join ',')）：脚本不会动用户实例，已退出。"; exit 1 }
Get-Process -Name 'dsh-lab*','dsh-launch-console','dsh-browser' -ErrorAction SilentlyContinue | Stop-Process -Force
Get-NetTCPConnection -LocalPort 3199 -State Listen -ErrorAction SilentlyContinue |
  ForEach-Object { Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue }
Start-Sleep -Milliseconds 500

@'
const http = require('http'), fs = require('fs');
const dir = process.env.TEMP + '\\perm-test';
const token = 'PERMTOKEN123456';
setTimeout(() => console.log('dsh web: http://127.0.0.1:3199/?token=' + token), 150);
const page = `<!doctype html><html><head><meta charset="utf-8"><title>PERM-TEST</title></head><body>
<script>
(async () => {
  let perm = 'error';
  try {
    if (typeof Notification === 'undefined') { perm = 'no-api'; }
    else { perm = Notification.permission; if (perm === 'default') { perm = await Notification.requestPermission(); } }
  } catch (e) { perm = 'throw:' + e; }
  fetch('/perm?p=' + encodeURIComponent(perm));
})();
</script></body></html>`;
http.createServer((q, s) => {
  fs.appendFileSync(dir + '\\req.txt', Date.now() + ' ' + q.url + '\n');
  if (q.url.indexOf('/perm') === 0) { s.writeHead(200); s.end('ok'); return; }
  if (q.url.indexOf('token=' + token) >= 0) { s.writeHead(200, {'Content-Type':'text/html; charset=utf-8'}); s.end(page); return; }
  s.writeHead(401); s.end('unauthorized');
}).listen(3199);
'@ | Set-Content -Encoding ASCII "$T\mock.js"
@"
@echo off
node "$T\mock.js"
"@ | Set-Content -Encoding ASCII "$T\npx.cmd"

$env:DSH_LAUNCH_CONSOLE_URL = 'http://127.0.0.1:3199'
$env:DSH_LAUNCH_CONSOLE_NPX = "$T\npx.cmd"
$env:DSH_LAUNCH_CONSOLE_STATEDIR = "$T\state"
$env:DSH_LAUNCH_CONSOLE_NOMSGBOX = '1'
$env:PATH = "$T;C:\Program Files\nodejs;C:\Windows\system32;C:\Windows"
$p = Start-Process -FilePath "$PROJ\target\release\dsh-launch-console.exe" -PassThru
for ($i = 0; $i -lt 100; $i++) {
  Start-Sleep -Milliseconds 100
  if (Test-Path "$T\req.txt") { if ((Get-Content "$T\req.txt" -Raw) -match '/perm') { break } }
}
Start-Sleep -Milliseconds 800
Write-Output "--- mock 收到的请求 ---"
Get-Content "$T\req.txt" -ErrorAction SilentlyContinue
Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 600
