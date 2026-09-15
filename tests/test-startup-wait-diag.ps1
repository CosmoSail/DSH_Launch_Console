# DSH 启动「卡在哪」诊断：观测启动期间的 TCP 连接状态
#
# 用途：在 DSH 启动期间持续抓该进程的 TCP 连接，判断启动是否阻塞在网络等待上。
# 实测结论：DSH 启动期间没有任何非监听 TCP 连接 —— 它是纯 CPU 密集的本地操作
#           （CPU 时间 ≈ 墙钟时间）；"启动卡住" 的观感来自下面记录的探测 bug，
#           并非真实阻塞。
#
# 踩坑记录：早期版本的端口探测函数写成 param([int]$P) 却用 -Port 调用，PowerShell
#           不会报错，而是把 $P 静默绑成 0，于是探测恒为假、被误判成"启动卡住"。
#           本版改用 netstat 判定监听状态。

param(
  [int]$Port = 3180,
  [int]$MaxSeconds = 90,
  [string]$DshBin
)

$ErrorActionPreference = 'Stop'
$NODE    = (Get-Command node -ErrorAction SilentlyContinue).Source
# 全局 dsh 包入口：-DshBin 优先，否则用 npm root -g 推导
$DSH_BIN = if ($DshBin) { $DshBin } else {
  $r = & npm root -g 2>$null
  if ($r) { Join-Path $r '@deepseek-ai\dsh\lib\bin.js' } else { '' }
}
$WORK    = Join-Path $env:TEMP 'dsh-lock-diag'
Remove-Item -Recurse -Force $WORK -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $WORK | Out-Null

if ($Port -eq 3080) { throw "拒绝使用 3080" }
if (-not $NODE)                { throw "未找到 node，请先安装 Node.js" }
if (-not (Test-Path $DSH_BIN)) { throw "找不到全局 dsh 入口（先 npm i -g @deepseek-ai/dsh），或用 -DshBin 指定；当前：$DSH_BIN" }

# 用 netstat 抓某个 pid 的全部 TCP 行（比 Get-NetTCPConnection 轻得多）
function Get-TcpLines {
  param([int]$ProcessId)
  $raw = netstat -ano 2>$null
  $out = @()
  foreach ($l in $raw) {
    if ($l -match '^\s*TCP\s') {
      $f = $l.Trim() -split '\s+'
      if ($f.Count -ge 5 -and $f[4] -eq "$ProcessId") {
        $out += [pscustomobject]@{ Local = $f[1]; Remote = $f[2]; State = $f[3] }
      }
    }
  }
  return $out
}

Write-Output "=== DSH 启动「等待定位」port=$Port，最长 $MaxSeconds 秒 ==="
Write-Output ("护航：3080 owner = {0}" -f ((Get-NetTCPConnection -LocalPort 3080 -State Listen -ErrorAction SilentlyContinue).OwningProcess -join ','))
Write-Output ""

$out = Join-Path $WORK 'out.log'; $err = Join-Path $WORK 'err.log'
$sw = [System.Diagnostics.Stopwatch]::StartNew()
$p = Start-Process -FilePath $NODE `
      -ArgumentList @($DSH_BIN, 'web', '--host', '127.0.0.1', '--port', "$Port", '--no-open') `
      -WorkingDirectory $env:USERPROFILE `
      -RedirectStandardOutput $out -RedirectStandardError $err `
      -PassThru -WindowStyle Hidden

$readyMs = $null
$timeline = @()
$lastState = ''
while ($sw.Elapsed.TotalSeconds -lt $MaxSeconds) {
  $conns = Get-TcpLines -ProcessId $p.Id
  $listening = $conns | Where-Object { $_.State -eq 'LISTENING' -and $_.Local -match ":$Port$" }
  if ($listening) { $readyMs = [int]$sw.Elapsed.TotalMilliseconds; break }

  # 只记录「状态发生变化」的时刻，避免刷屏
  $remote = ($conns | Where-Object { $_.State -ne 'LISTENING' } |
             ForEach-Object { "$($_.Remote)/$($_.State)" } | Sort-Object) -join ' '
  if ($remote -ne $lastState) {
    $timeline += [pscustomobject]@{ T = [int]$sw.Elapsed.TotalMilliseconds; Info = $remote }
    $lastState = $remote
  }
  Start-Sleep -Milliseconds 400
}
$sw.Stop()

Write-Output "--- TCP 连接时间线（只记变化）---"
if ($timeline.Count -eq 0) { Write-Output "  (启动期间该进程没有任何非监听 TCP 连接)" }
foreach ($e in $timeline) { Write-Output ("  t={0,7}ms  {1}" -f $e.T, $e.Info) }

Write-Output ""
Write-Output "--- 结果 ---"
if ($readyMs) {
  Write-Output ("  端口 {0} 就绪于: {1} ms" -f $Port, $readyMs)
} else {
  Write-Output ("  {0} 秒内未监听端口 {1}" -f $MaxSeconds, $Port)
}
$st = Get-CimInstance Win32_Process -Filter "ProcessId=$($p.Id)" -ErrorAction SilentlyContinue
if ($st) {
  Write-Output ("  进程 CPU 时间: {0:N0} ms   读取: {1:N1} MB   工作集: {2:N0} MB" -f `
    (($st.UserModeTime + $st.KernelModeTime)/10000), ($st.ReadTransferCount/1MB), ($st.WorkingSetSize/1MB))
}
Write-Output ""
Write-Output "--- 子进程 ---"
Get-CimInstance Win32_Process -Filter "ParentProcessId=$($p.Id)" -ErrorAction SilentlyContinue |
  ForEach-Object { Write-Output ("  {0} {1}" -f $_.ProcessId, $_.Name) }
Write-Output ""
Write-Output "--- stderr ---"; Get-Content $err -ErrorAction SilentlyContinue | Select-Object -First 12
Write-Output "--- stdout ---"; Get-Content $out -ErrorAction SilentlyContinue | Select-Object -First 5

taskkill /PID $p.Id /T /F 2>&1 | Out-Null
Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue |
  ForEach-Object { taskkill /PID $_.OwningProcess /T /F 2>&1 | Out-Null }
Write-Output ""
Write-Output "（已清理；明细在 $WORK）"
