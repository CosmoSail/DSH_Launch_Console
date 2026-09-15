# DSH 启动画像：磁盘 IO 量 / CPU 时间 / 墙钟时间
#
# 目的：判断 DSH 启动的瓶颈是「磁盘 IO」还是「CPU 解析」。
#   判据：CPU 时间（User+Kernel）≈ 墙钟时间  → CPU 受限
#         CPU 时间 << 墙钟时间                → IO 受限 / 等待外部
#
# 只读测量，不改任何配置；只用 3180 端口，绝不触碰 3080。

param(
  [int]$Port = 3180,
  [string]$DshBin
)

$ErrorActionPreference = 'Stop'
$NODE = (Get-Command node -ErrorAction SilentlyContinue).Source
# 全局 dsh 包入口：-DshBin 优先，否则用 npm root -g 推导
$DSH_BIN = if ($DshBin) { $DshBin } else {
  $r = & npm root -g 2>$null
  if ($r) { Join-Path $r '@deepseek-ai\dsh\lib\bin.js' } else { '' }
}
$WORK = Join-Path $env:TEMP 'dsh-profile-measure'
Remove-Item -Recurse -Force $WORK -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $WORK | Out-Null

if ($Port -eq 3080) { throw "拒绝使用 3080" }
if (-not $NODE)                { throw "未找到 node，请先安装 Node.js" }
if (-not (Test-Path $DSH_BIN)) { throw "找不到全局 dsh 入口（先 npm i -g @deepseek-ai/dsh），或用 -DshBin 指定；当前：$DSH_BIN" }

function Test-Port {
  param([int]$Port, [int]$TimeoutMs = 250)
  $c = New-Object System.Net.Sockets.TcpClient
  try {
    $iar = $c.BeginConnect('127.0.0.1', $Port, $null, $null)
    if ($iar.AsyncWaitHandle.WaitOne($TimeoutMs)) { $c.EndConnect($iar); return $true }
    return $false
  } catch { return $false } finally { $c.Close() }
}

# 取一个进程及其直接子进程的累计快照
# （DSH 的 MCP server 是直接子进程；更深的层级影响很小，故只取两层）
function Get-TreeStats {
  param([int]$RootPid)
  $procs = @(Get-CimInstance Win32_Process -Filter "ProcessId=$RootPid" -ErrorAction SilentlyContinue)
  $procs += @(Get-CimInstance Win32_Process -Filter "ParentProcessId=$RootPid" -ErrorAction SilentlyContinue)
  $read = 0L; $write = 0L; $cpu = 0L; $ws = 0L; $count = 0
  foreach ($x in $procs) {
    if ($null -eq $x) { continue }
    $read  += [long]$x.ReadTransferCount
    $write += [long]$x.WriteTransferCount
    $cpu   += ([long]$x.UserModeTime + [long]$x.KernelModeTime)
    $ws    += [long]$x.WorkingSetSize
    $count++
  }
  return [pscustomobject]@{ Read = $read; Write = $write; CpuMs = $cpu / 10000; Ws = $ws; Count = $count }
}

Write-Output "=== DSH 启动画像（直启 node，port=$Port）==="
$out = Join-Path $WORK 'out.log'; $err = Join-Path $WORK 'err.log'
$sw = [System.Diagnostics.Stopwatch]::StartNew()
$p = Start-Process -FilePath $NODE `
      -ArgumentList @($DSH_BIN, 'web', '--host', '127.0.0.1', '--port', "$Port", '--no-open') `
      -WorkingDirectory $env:USERPROFILE `
      -RedirectStandardOutput $out -RedirectStandardError $err `
      -PassThru -WindowStyle Hidden

# 采样到端口就绪
$samples = @()
while ($sw.Elapsed.TotalSeconds -lt 120) {
  if (Test-Port -Port $Port) { break }
  $st = Get-TreeStats -RootPid $p.Id
  $samples += [pscustomobject]@{ T = [int]$sw.Elapsed.TotalMilliseconds; ReadMB = $st.Read/1MB; CpuMs = $st.CpuMs; Procs = $st.Count }
  Start-Sleep -Milliseconds 300
}
$ready = [int]$sw.Elapsed.TotalMilliseconds
$final = Get-TreeStats -RootPid $p.Id
$sw.Stop()

Write-Output ""
Write-Output ("到端口就绪（墙钟）    : {0} ms" -f $ready)
Write-Output ("  进程树 CPU 时间     : {0:N0} ms   ({1:P0} 的墙钟)" -f $final.CpuMs, ($final.CpuMs / [math]::Max($ready,1)))
Write-Output ("  进程树 读取字节     : {0:N1} MB" -f ($final.Read/1MB))
Write-Output ("  进程树 写入字节     : {0:N1} MB" -f ($final.Write/1MB))
Write-Output ("  进程数 / 工作集     : {0} 个 / {1:N0} MB" -f $final.Count, ($final.Ws/1MB))
Write-Output ""
Write-Output "--- 采样曲线（每 300ms）---"
$lastT = 0
foreach ($s in $samples) {
  if ($s.T - $lastT -lt 900 -and $s.T -lt $ready - 900) { continue }
  $lastT = $s.T
  Write-Output ("  t={0,6}ms  read={1,7:N1}MB  cpu={2,7:N0}ms  进程={3}" -f $s.T, $s.ReadMB, $s.CpuMs, $s.Procs)
}

taskkill /PID $p.Id /T /F 2>&1 | Out-Null
Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue |
  ForEach-Object { taskkill /PID $_.OwningProcess /T /F 2>&1 | Out-Null }
Write-Output ""
Write-Output "（进程已清理）"
