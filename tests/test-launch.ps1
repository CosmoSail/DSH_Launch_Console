# 启动链重构验证：直连包入口 / 失败分类 / npx 回退
# 安全约定：只用改名副本 dsh-lab.exe；只按本脚本记录的 PID 结束进程；绝不碰 DSH Launch Console。
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
$LABEXE = "$PSScriptRoot\dsh-lab.exe"
$NODE = 'C:\Program Files\nodejs\node.exe'
$SYS = "C:\Windows\system32;C:\Windows"
$PORT = 3198
$TOKEN = 'LABTOKEN123456'
$serverLog = Join-Path $env:TEMP 'DSH-Launch-Console-server.log'
$launcherLog = Join-Path $env:TEMP 'DSH-Launch-Console.log'

# 使用独立互斥体名 + 独立状态目录：可与用户正在运行的实例并存，且绝不动它
$live = Get-Process -Name DSH_Launch_Console -ErrorAction SilentlyContinue
if ($live) { Write-Output ("注意：用户实例运行中（pid " + ($live.Id -join ',') + "），本脚本用独立互斥体并存。") }
$env:DSH_LAUNCH_CONSOLE_MUTEX = 'DSH_Launch_Console_LabInstance_Mutex'
Copy-Item $EXE $LABEXE -Force

function New-Lab {
  param([string]$Name)
  $T = Join-Path $env:TEMP "lab-$Name"
  Remove-Item -Recurse -Force $T -ErrorAction SilentlyContinue
    Copy-Item $NODE (Join-Path $T 'node.exe') -Force
  # npm 生成的 dsh.cmd shim（真实格式：%dp0% + 内嵌 lib/bin.js）
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
  '{"name":"@deepseek-ai/dsh","version":"0.0.0-lab","bin":{"dsh":"lib/bin.js"}}' |
    Set-Content -Encoding ASCII (Join-Path $T 'node_modules\@deepseek-ai\dsh\package.json')
  return @{ Dir = $T; Lib = $lib }
}

function Read-New([string]$path, [int]$from) {
  if (-not (Test-Path $path)) { return @() }
  $all = Get-Content $path
  if ($all.Count -le $from) { return @() }
  return $all[$from..($all.Count - 1)]
}

########################################################################
Write-Output "=== 用例 1：直连包入口（node <entry> web --host --port --no-open）==="
$lab = New-Lab 'entry'
$mockBody = @'
const fs = require('fs'), http = require('http'), path = require('path');
const root = path.resolve(__dirname, '..', '..', '..', '..');
const args = process.argv.slice(2);
fs.writeFileSync(path.join(root, 'args.txt'), JSON.stringify(process.argv.slice(1)));
const pi = args.indexOf('--port'), hi = args.indexOf('--host');
const port = pi >= 0 ? parseInt(args[pi + 1], 10) : 3198;
const host = hi >= 0 ? args[hi + 1] : '127.0.0.1';
const token = 'LABTOKEN123456';
const t0 = Date.now();
setTimeout(() => console.log('dsh web: http://' + host + ':' + port + '/?token=' + token), 200);
http.createServer((q, s) => {
  fs.appendFileSync(path.join(root, 'req.txt'), (Date.now() - t0) + ' ' + q.url + '\n');
  if (q.url.indexOf('token=' + token) >= 0) { s.writeHead(200, {'Content-Type':'text/html; charset=utf-8'}); s.end('<title>LAB-DSH</title>lab entry ok'); }
  else { s.writeHead(401); s.end('unauthorized'); }
}).listen(port, host);
'@
Set-Content -Encoding ASCII (Join-Path $lab.Lib 'bin.js') $mockBody
Remove-Item $serverLog, $launcherLog -Force -ErrorAction SilentlyContinue

$env:DSH_LAUNCH_CONSOLE_URL = "http://127.0.0.1:$PORT"
$env:DSH_LAUNCH_CONSOLE_NOMSGBOX = '1'
$env:PATH = "$($lab.Dir);$SYS"      # 只有实验室目录 + 系统目录
Remove-Item Env:DSH_LAUNCH_CONSOLE_NPX -ErrorAction SilentlyContinue

$p = Start-Process -FilePath $LABEXE -PassThru
$ok = $false
for ($i = 0; $i -lt 200; $i++) {
  Start-Sleep -Milliseconds 100
  if (Test-Path "$($lab.Dir)\req.txt") { if ((Get-Content "$($lab.Dir)\req.txt" -Raw) -match 'token=') { $ok = $true; break } }
}
Start-Sleep -Milliseconds 500
Write-Output ("  带 token 请求成功: " + $ok)
if (Test-Path "$($lab.Dir)\args.txt") { Write-Output ("  子进程收到的参数: " + (Get-Content "$($lab.Dir)\args.txt" -Raw).Trim()) }
Write-Output "  启动器日志:"
Select-String -Path $launcherLog -Pattern 'starting:|web UI is up|applied web token|window shown' | ForEach-Object { Write-Output ("    " + ($_ -replace '^\[\d+\] ', '')) }
Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 1
$left = Get-NetTCPConnection -LocalPort $PORT -State Listen -ErrorAction SilentlyContinue
Write-Output ("  退出后端口 $PORT 已释放: " + ($null -eq $left))

########################################################################
Write-Output ""
Write-Output "=== 用例 2：失败分类（entry 报 EADDRINUSE）==="
$lab2 = New-Lab 'eaddr'
@'
console.error("node:events:496");
console.error("Error: listen EADDRINUSE: address already in use 127.0.0.1:3198");
process.exit(1);
'@ | Set-Content -Encoding ASCII (Join-Path $lab2.Lib 'bin.js')
Remove-Item $serverLog, $launcherLog -Force -ErrorAction SilentlyContinue
$env:PATH = "$($lab2.Dir);$SYS"
$p2 = Start-Process -FilePath $LABEXE -PassThru
Start-Sleep -Seconds 5
Write-Output "  启动器日志:"
Get-Content $launcherLog -ErrorAction SilentlyContinue | ForEach-Object { Write-Output ("    " + ($_ -replace '^\[\d+\] ', '')) }
Stop-Process -Id $p2.Id -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 800

########################################################################
Write-Output ""
Write-Output "=== 用例 3：解析不到全局包 → npx 回退（参数仍带 host/port）==="
$lab3 = New-Lab 'npxfall'
Remove-Item (Join-Path $lab3.Dir 'dsh.cmd') -Force            # PATH 上没有 dsh
@"
@echo off
echo %* > "$($lab3.Dir)\npx-args.txt"
node "$($lab3.Dir)\mock.js"
"@ | Set-Content -Encoding ASCII (Join-Path $lab3.Dir 'npx.cmd')
$mockBody2 = $mockBody.Replace('LABTOKEN123456', $TOKEN)
Set-Content -Encoding ASCII (Join-Path $lab3.Dir 'mock.js') $mockBody2
Remove-Item $serverLog, $launcherLog -Force -ErrorAction SilentlyContinue
# 屏蔽真实全局安装位置，确保走到 npx 回退分支
$env:APPDATA = "$($lab3.Dir)\no-appdata"
$env:LOCALAPPDATA = "$($lab3.Dir)\no-localappdata"
$env:NVM_HOME = "$($lab3.Dir)\no-nvm"
$env:PATH = "$($lab3.Dir);$SYS"
$p3 = Start-Process -FilePath $LABEXE -PassThru
$ok3 = $false
for ($i = 0; $i -lt 200; $i++) {
  Start-Sleep -Milliseconds 100
  if (Test-Path "$($lab3.Dir)\npx-args.txt") { $ok3 = $true; break }
}
Start-Sleep -Milliseconds 800
Write-Output ("  npx 回退被调用: " + $ok3)
if (Test-Path "$($lab3.Dir)\npx-args.txt") { Write-Output ("  npx 收到: " + (Get-Content "$($lab3.Dir)\npx-args.txt" -Raw).Trim()) }
Write-Output "  启动器日志:"
Select-String -Path $launcherLog -Pattern 'entry resolution failed|starting:|probed' | ForEach-Object { Write-Output ("    " + ($_ -replace '^\[\d+\] ', '')) }
Stop-Process -Id $p3.Id -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 1

Write-Output ""
Write-Output "=== 清理 ==="
Remove-Item $LABEXE -Force -ErrorAction SilentlyContinue
Get-NetTCPConnection -LocalPort $PORT -State Listen -ErrorAction SilentlyContinue |
  ForEach-Object { Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue }
Write-Output "  残留 lab 目录:"
Get-ChildItem "$env:TEMP" -Directory -Filter 'lab-*' -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Name


# ── 收尾：还原用户设置（详见顶部「设置文件护栏」）─────────────────
Restore-TempState
