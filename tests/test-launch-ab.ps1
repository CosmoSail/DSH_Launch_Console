# A/B：新直连链路 vs 旧"隐藏脚本 + shim"链路（同一台机器、同一入口）
# 指标：进程启动 → mock 服务开始监听 的毫秒数
$ErrorActionPreference = 'Stop'

# ── 设置文件护栏 ────────────────────────────────────────────────
# 0.2.x 的设置只有一个真实位置：%TEMP%\DSH-Launch-Console-settings.json
# （没有 DSH_LAUNCH_CONSOLE_STATEDIR，也没有自管数据目录）。启动器启动时只读它，
# 只有界面/托盘里改设置才会写。本脚本跑之前把用户那份挪走，结束后原样还原，
# 保证测试用的端口 / profile 不会变成用户下次启动时的默认值。
$TempSettings = Join-Path $env:TEMP 'DSH-Launch-Console-settings.json'
$TempSettingsBak = "$TempSettings.bak.test"
$script:HadUserSettings = Test-Path $TempSettings
if ($script:HadUserSettings) { Copy-Item $TempSettings $TempSettingsBak -Force }
function Restore-TempState {
  if ($script:HadUserSettings) { Move-Item $TempSettingsBak $TempSettings -Force -ErrorAction SilentlyContinue }
  else { Remove-Item $TempSettings -Force -ErrorAction SilentlyContinue }
}
# ────────────────────────────────────────────────────────────────
# 路径约定：本脚本位于 <仓库根>\tests\，$ROOT 取其父目录即仓库根，$PSScriptRoot 即本目录
$ROOT = Split-Path $PSScriptRoot -Parent
$PROJ = $ROOT
$EXE = "$PROJ\target\release\dsh-launch-console.exe"
$LABEXE = "$PSScriptRoot\dsh-lab-new.exe"
$LABEXE_OLD = "$PSScriptRoot\dsh-lab-old.exe"
$NODE = 'C:\Program Files\nodejs\node.exe'
$SYS = "C:\Windows\system32;C:\Windows"
$PORT = 3198
$T = Join-Path $env:TEMP 'lab-ab'
Copy-Item $EXE $LABEXE -Force
Copy-Item "$ROOT\DSH_Launch_Console.exe" $LABEXE_OLD -Force

# 独立互斥体：与用户实例并存
$env:DSH_LAUNCH_CONSOLE_MUTEX = 'DSH_Launch_Console_LabInstance_Mutex'
Get-NetTCPConnection -LocalPort $PORT -State Listen -ErrorAction SilentlyContinue |
  ForEach-Object { Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue }

Remove-Item -Recurse -Force $T -ErrorAction SilentlyContinue
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
const args = process.argv.slice(2);
const pi = args.indexOf('--port'), hi = args.indexOf('--host');
const port = pi >= 0 ? parseInt(args[pi + 1], 10) : 3198;
const host = hi >= 0 ? args[hi + 1] : '127.0.0.1';
const token = 'ABTOKEN123456';
setTimeout(() => console.log('dsh web: http://' + host + ':' + port + '/?token=' + token), 150);
const srv = http.createServer((q, s) => {
  if (q.url.indexOf('token=' + token) >= 0) { s.writeHead(200, {'Content-Type':'text/html'}); s.end('<title>AB</title>ok'); }
  else { s.writeHead(401); s.end('no'); }
});
srv.listen(port, host, () => fs.writeFileSync(path.join(root, 'listen.txt'), String(Date.now())));
'@ | Set-Content -Encoding ASCII (Join-Path $lib 'bin.js')

$env:DSH_LAUNCH_CONSOLE_URL = "http://127.0.0.1:$PORT"
$env:DSH_LAUNCH_CONSOLE_NOMSGBOX = '1'
$env:PATH = "$T;$SYS"

function Measure-Arm([string]$label, [string]$override, [string]$exe, [int]$runs = 4) {
  $times = @()
  for ($i = 0; $i -lt $runs; $i++) {
    Remove-Item "$T\listen.txt" -Force -ErrorAction SilentlyContinue
    if ($override) { $env:DSH_LAUNCH_CONSOLE_NPX = $override } else { Remove-Item Env:DSH_LAUNCH_CONSOLE_NPX -ErrorAction SilentlyContinue }
    $t0 = Get-Date
    $p = Start-Process -FilePath $exe -PassThru
    for ($j = 0; $j -lt 300; $j++) {
      Start-Sleep -Milliseconds 10
      if (Test-Path "$T\listen.txt") { break }
    }
    $ms = [int]((Get-Date) - $t0).TotalMilliseconds
    if (Test-Path "$T\listen.txt") { $times += $ms } else { Write-Output "  ${label}: 第 $i 次未就绪" }
    Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 900
  }
  if ($times.Count -gt 0) {
    $sorted = $times | Sort-Object
    Write-Output ("{0,-28} min={1,5}ms  median={2,5}ms  all={3}" -f $label, $sorted[0], $sorted[[int]($sorted.Count / 2)], ($times -join ','))
  }
}

Write-Output "=== 启动到服务监听的耗时（含 PowerShell Start-Process 固定开销）==="
Measure-Arm 'A 新版：进程内解析+直连 node' '' $LABEXE
Measure-Arm 'B 旧版：where 探测+脚本+shim' '' $LABEXE_OLD

Remove-Item $LABEXE,$LABEXE_OLD -Force -ErrorAction SilentlyContinue
Get-NetTCPConnection -LocalPort $PORT -State Listen -ErrorAction SilentlyContinue |
  ForEach-Object { Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue }


# ── 收尾：还原用户设置（详见顶部「设置文件护栏」）─────────────────
Restore-TempState
