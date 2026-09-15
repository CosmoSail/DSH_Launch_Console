# 启动速度对比测试：
#   A 旧版 exe + 假 npx（模拟 npx 固定开销 ~2s）→ 基线
#   B 新版 exe + 假 dsh（PATH 命中直连快路径）→ 新行为
#   C 新版 exe + 只有假 npx（PATH 无 dsh）→ 回退分支
# 指标：进程启动 → 浏览器发出「带 token 的首个 HTTP 请求」的毫秒数（≈ 用户可用的时刻）。
$ErrorActionPreference = 'Stop'
# 路径约定：本脚本位于 <仓库根>\tests\，$ROOT 取其父目录即仓库根，$PSScriptRoot 即本目录
$ROOT = Split-Path $PSScriptRoot -Parent
$PROJ = $ROOT
$OLD = "$ROOT\DSH_Launch_Console.exe"
$NEW = "$PROJ\target\release\dsh-launch-console.exe"
$NODE_DIR = 'C:\Program Files\nodejs'
$SYS = "C:\Windows\system32;C:\Windows"
$TOKEN = 'TESTTOKEN123456'
$serverLog = Join-Path $env:TEMP 'DSH-Launch-Console-server.log'
$launcherLog = Join-Path $env:TEMP 'DSH-Launch-Console.log'

$mockJs = @'
const http = require('http'), fs = require('fs');
const dir = process.env.MOCK_DIR;
const token = process.env.MOCK_TOKEN;
const t0 = Date.now();
// 模拟 dsh 打印 token 行（启动器从这份日志里抓 token）
setTimeout(() => {
  console.log('dsh web: http://127.0.0.1:3199/?token=' + token);
}, 150);
http.createServer((q, s) => {
  const ok = q.url.indexOf('token=' + token) >= 0;
  fs.appendFileSync(dir + '\\req.txt', (Date.now() - t0) + ' ' + (ok ? 'TOKEN' : 'PLAIN') + ' ' + q.url + '\n');
  if (ok) { s.writeHead(200, {'Content-Type': 'text/html'}); s.end('<title>MOCK-DSH</title>mock'); }
  else { s.writeHead(401, {'Content-Type': 'text/html'}); s.end('unauthorized'); }
}).listen(3199);
'@

function Run-Case {
  param([string]$Name, [string]$Exe, [string]$LauncherKind, [int]$FakeDelaySec)
  Write-Output "=== $Name ==="
  $T = Join-Path $env:TEMP ("fp-" + $Name)
  Remove-Item -Recurse -Force $T -ErrorAction SilentlyContinue
  New-Item -ItemType Directory -Force -Path $T, "$T\state" | Out-Null
  Remove-Item $serverLog, $launcherLog -Force -ErrorAction SilentlyContinue
  # 安全护栏：绝不强杀用户正在运行的 DSH Launch Console 实例（只清理实验室副本）
$live = Get-Process -Name DSH_Launch_Console -ErrorAction SilentlyContinue
if ($live) { Write-Output "!! DSH Launch Console 正在运行（pid $($live.Id -join ',')）：脚本不会动用户实例，已退出。"; exit 1 }
Get-Process -Name 'dsh-lab*','dsh-launch-console','dsh-browser' -ErrorAction SilentlyContinue | Stop-Process -Force
  Get-NetTCPConnection -LocalPort 3199 -State Listen -ErrorAction SilentlyContinue |
    ForEach-Object { Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue }
  Start-Sleep -Milliseconds 500

  Set-Content -Encoding ASCII (Join-Path $T 'mock.js') $mockJs
  $delay = if ($FakeDelaySec -gt 0) { "ping -n $($FakeDelaySec + 1) 127.0.0.1 >nul`r`n" } else { '' }
  if ($LauncherKind -eq 'dsh') {
    Set-Content -Encoding ASCII (Join-Path $T 'dsh.cmd') "@echo off`r`necho MARKER args=%* > `"$T\marker.txt`"`r`n${delay}node `"$T\mock.js`""
  } else {
    Set-Content -Encoding ASCII (Join-Path $T 'npx.cmd') "@echo off`r`necho MARKER args=%* > `"$T\marker.txt`"`r`n${delay}node `"$T\mock.js`""
  }

  $env:DSH_LAUNCH_CONSOLE_URL = 'http://127.0.0.1:3199'
  $env:DSH_LAUNCH_CONSOLE_STATEDIR = "$T\state"
  $env:DSH_LAUNCH_CONSOLE_NOMSGBOX = '1'
  $env:MOCK_DIR = $T
  $env:MOCK_TOKEN = $TOKEN
  $env:PATH = "$T;$NODE_DIR;$SYS"   # 只有本用例的假启动器 + node + 系统目录（无真 dsh/npx）

  $t0 = Get-Date
  $p = Start-Process -FilePath $Exe -PassThru
  $delta = $null
  for ($i = 0; $i -lt 400; $i++) {
    Start-Sleep -Milliseconds 50
    if (Test-Path (Join-Path $T 'req.txt')) {
      $lines = Get-Content (Join-Path $T 'req.txt')
      $hit = $lines | Where-Object { $_ -match ' TOKEN ' } | Select-Object -First 1
      if ($hit) { $delta = [int]($hit -split ' ')[0]; break }
    }
  }
  $elapsed = [int]((Get-Date) - $t0).TotalMilliseconds
  if ($delta -ne $null) {
    Write-Output ("  首个带 token 请求: mock 计时 {0}ms（进程外计时 {1}ms）" -f $delta, $elapsed)
  } else {
    Write-Output ("  !! 60 秒内没有带 token 的请求（进程外计时 {0}ms）" -f $elapsed)
  }
  $marker = Join-Path $T 'marker.txt'
  if (Test-Path $marker) { Write-Output ("  启动器命中: " + ((Get-Content $marker) -join ' | ')) }
  else { Write-Output "  !! 启动器没有被调用" }
  if (Test-Path $serverLog) {
    Get-Content $serverLog | Where-Object { $_ -match '\[launch\]|starting|exited' } |
      ForEach-Object { Write-Output ("  会话日志: " + $_) }
  }
  if (Test-Path $launcherLog) {
    Get-Content $launcherLog | Where-Object { $_ -match 'starting:|web UI is up|window shown|applied web token|token ready' } |
      ForEach-Object { Write-Output ("  启动器日志: " + $_) }
  }
  Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
  Start-Sleep -Milliseconds 1200
  $alive = Get-NetTCPConnection -LocalPort 3199 -State Listen -ErrorAction SilentlyContinue
  Write-Output ("  清理: 端口 3199 已关闭 = " + ($null -eq $alive))
  Write-Output ""
}

Run-Case -Name 'A-old-npx' -Exe $OLD -LauncherKind 'npx' -FakeDelaySec 2
Run-Case -Name 'B-new-dsh' -Exe $NEW -LauncherKind 'dsh' -FakeDelaySec 0
Run-Case -Name 'C-new-fallback' -Exe $NEW -LauncherKind 'npx' -FakeDelaySec 0
