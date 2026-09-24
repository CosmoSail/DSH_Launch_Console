# DSH 启动耗时对照实验：DSH 直启 vs 经 DSH Launch Console 启动器启动
#
# 目的：回答「启动慢是 DSH 自身固有开销，还是启动器造成的」。
#
# 方法：同一台机器、同一份 DSH_HOME（真实配置）、同一套 dsh web 参数，
#       只改变「谁去 spawn node」，测量到 127.0.0.1:<port> 可连接为止的毫秒数。
#   A 组：node <bin.js> web --host 127.0.0.1 --port <直接端口> --no-open
#         —— 无启动器、无 WebView2、无窗口，代表 DSH 固有时延下限。
#   B 组：DSH_Launch_Console.exe（DSH_LAUNCH_CONSOLE_URL 指向 <启动器端口>）
#         —— 完整启动器路径：解析入口 + 直连 node + 同步建 WebView2 窗口。
#
# 安全约定：
#   * 绝不使用 3080（那是承载当前 DSH 会话的端口），绝不 taskkill /IM node.exe；
#   * 只按本脚本自己记录的 PID 用 taskkill /PID <pid> /T /F 结束进程树；
#   * 启动器一律用独立互斥体 + 独立状态目录，绝不碰用户正在运行的实例。
#
# 用法：pwsh -File .\test-startup-timing.ps1 [-Runs 2]

param(
  [int]$Runs          = 2,
  [int]$PortDirect    = 3180,
  [int]$PortLauncher  = 3181,
  [string]$DshBin
)

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
$ROOT    = Split-Path $PSScriptRoot -Parent
$NODE    = (Get-Command node -ErrorAction SilentlyContinue).Source
$EXE     = Join-Path $ROOT 'target\release\dsh-launch-console.exe'
$WORK    = Join-Path $env:TEMP 'dsh-timing'
$LAUNCHER_LOG = Join-Path $env:TEMP 'DSH-Launch-Console.log'
# 全局 dsh 包入口：-DshBin 优先，否则用 npm root -g 推导
$DSH_BIN = if ($DshBin) { $DshBin } else {
  $r = & npm root -g 2>$null
  if ($r) { Join-Path $r '@deepseek-ai\dsh\lib\bin.js' } else { '' }
}

# ---- 前置检查 --------------------------------------------------------------
foreach ($p in @($PortDirect, $PortLauncher)) {
  if ($p -eq 3080) { throw "拒绝使用 3080：那是当前 DSH 会话的端口" }
}
if (-not $NODE)                { throw "未找到 node，请先安装 Node.js" }
if (-not (Test-Path $DSH_BIN)) { throw "找不到全局 dsh 入口（先 npm i -g @deepseek-ai/dsh），或用 -DshBin 指定；当前：$DSH_BIN" }
if (-not (Test-Path $EXE))     { throw "找不到启动器：$EXE（先 cargo build --release）" }

$liveOwner = Get-NetTCPConnection -LocalPort 3080 -State Listen -ErrorAction SilentlyContinue |
             Select-Object -ExpandProperty OwningProcess -First 1
Write-Output ("护航：3080 当前属于 pid {0}（本次实验全程不触碰）" -f $liveOwner)
Write-Output ""

Remove-Item -Recurse -Force $WORK -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $WORK | Out-Null

# ---- 工具函数 --------------------------------------------------------------
function Test-Port {
  param([int]$Port, [int]$TimeoutMs = 250)
  $c = New-Object System.Net.Sockets.TcpClient
  try {
    $iar = $c.BeginConnect('127.0.0.1', $Port, $null, $null)
    if ($iar.AsyncWaitHandle.WaitOne($TimeoutMs)) { $c.EndConnect($iar); return $true }
    return $false
  } catch { return $false } finally { $c.Close() }
}

function Clear-Port {
  param([int]$Port)
  Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue |
    ForEach-Object { taskkill /PID $_.OwningProcess /T /F 2>&1 | Out-Null }
}

function Kill-Tree {
  param([int]$ProcessId)
  if ($ProcessId -gt 0) { taskkill /PID $ProcessId /T /F 2>&1 | Out-Null }
}

# 轮询端口直到就绪，返回毫秒数（未就绪返回 $null）
function Wait-Port {
  param([int]$Port, [int]$TimeoutSec = 120)
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  while ($sw.Elapsed.TotalSeconds -lt $TimeoutSec) {
    if (Test-Port -Port $Port) { return [int]$sw.Elapsed.TotalMilliseconds }
    Start-Sleep -Milliseconds 25
  }
  return $null
}

$results = @()

# ---- A 组：DSH 直启（无启动器、无 WebView2）--------------------------------
Write-Output "=== A 组：node 直启 DSH（无启动器 / 无 WebView2）port=$PortDirect ==="
for ($i = 1; $i -le $Runs; $i++) {
  Clear-Port -Port $PortDirect
  Start-Sleep -Milliseconds 600
  $out = Join-Path $WORK "A$i.out.log"
  $err = Join-Path $WORK "A$i.err.log"
  Remove-Item $out, $err -Force -ErrorAction SilentlyContinue

  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath $NODE `
        -ArgumentList @($DSH_BIN, 'web', '--host', '127.0.0.1', '--port', "$PortDirect", '--no-open') `
        -WorkingDirectory $env:USERPROFILE `
        -RedirectStandardOutput $out -RedirectStandardError $err `
        -PassThru -WindowStyle Hidden
  $ms = Wait-Port -Port $PortDirect
  $sw.Stop()

  if ($ms -ne $null) {
    Write-Output ("  第 {0} 次: {1,6} ms   (pid {2})" -f $i, $ms, $p.Id)
    $results += [pscustomobject]@{ Arm = 'A 直接启动 node'; Run = $i; Ms = $ms }
  } else {
    Write-Output ("  第 {0} 次: 超时未就绪 (pid {1})" -f $i, $p.Id)
  }
  Kill-Tree -ProcessId $p.Id
  Start-Sleep -Milliseconds 1200
  Clear-Port -Port $PortDirect
}
Write-Output ""

# ---- B 组：经 DSH Launch Console 启动器 -----------------------------------------
Write-Output "=== B 组：经 DSH Launch Console 启动器 port=$PortLauncher ==="
$env:DSH_LAUNCH_CONSOLE_URL      = "http://127.0.0.1:$PortLauncher"
$env:DSH_LAUNCH_CONSOLE_MUTEX    = 'DSH_Launch_Console_TimingTest_Mutex'
$env:DSH_LAUNCH_CONSOLE_NOMSGBOX = '1'
Remove-Item Env:DSH_LAUNCH_CONSOLE_NPX -ErrorAction SilentlyContinue

for ($i = 1; $i -le $Runs; $i++) {
  Clear-Port -Port $PortLauncher
  # 不删除用户日志：只记行数偏移，仅回显本轮新增的行
  $logBefore = if (Test-Path $LAUNCHER_LOG) { @(Get-Content $LAUNCHER_LOG).Count } else { 0 }
    Start-Sleep -Milliseconds 600

  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process -FilePath $EXE -PassThru
  $ms = Wait-Port -Port $PortLauncher
  $sw.Stop()

  if ($ms -ne $null) {
    Write-Output ("  第 {0} 次: {1,6} ms   (pid {2})" -f $i, $ms, $p.Id)
    $results += [pscustomobject]@{ Arm = 'B 经启动器'; Run = $i; Ms = $ms }
  } else {
    Write-Output ("  第 {0} 次: 超时未就绪 (pid {1})" -f $i, $p.Id)
  }
  if (Test-Path $LAUNCHER_LOG) {
    Get-Content $LAUNCHER_LOG | Select-Object -Skip $logBefore |
      Where-Object { $_ -match 'starting:|web UI is up|applied web token|window shown' } |
      ForEach-Object { Write-Output ("      " + ($_ -replace '^\[\d+\] ', '')) }
  }
  Kill-Tree -ProcessId $p.Id
  Start-Sleep -Milliseconds 1500
  Clear-Port -Port $PortLauncher
}

# ---- 汇总 ------------------------------------------------------------------
Write-Output ""
Write-Output "=== 汇总 ==="
foreach ($arm in @('A 直接启动 node', 'B 经启动器')) {
  $set = $results | Where-Object { $_.Arm -eq $arm }
  if ($set.Count -gt 0) {
    $sorted = $set.Ms | Sort-Object
    $med = $sorted[[int]($sorted.Count / 2)]
    Write-Output ("{0,-18} 次数={1}  min={2}ms  median={3}ms  all={4}" -f `
      $arm, $set.Count, $sorted[0], $med, ($set.Ms -join ','))
  }
}
$A = ($results | Where-Object Arm -eq 'A 直接启动 node').Ms
$B = ($results | Where-Object Arm -eq 'B 经启动器').Ms
if ($A.Count -gt 0 -and $B.Count -gt 0) {
  $am = ($A | Sort-Object)[[int]($A.Count / 2)]
  $bm = ($B | Sort-Object)[[int]($B.Count / 2)]
  Write-Output ""
  Write-Output ("中位数差（启动器 - 直接）= {0} ms" -f ($bm - $am))
}

# ---- 清理 ------------------------------------------------------------------
Clear-Port -Port $PortDirect
Clear-Port -Port $PortLauncher
Write-Output ""
Write-Output "清理完成；明细日志在 $WORK"

# ── 收尾：还原用户设置（详见顶部「设置文件护栏」）─────────────────
Restore-TempState
