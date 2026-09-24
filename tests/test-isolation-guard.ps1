# 单测：设置文件护栏的备份/还原逻辑（不启动启动器、不碰端口）
# 说明：护栏的各语句在真实脚本里都在**顶层**，所以这里也照顶层写 ——
# 若把 function Restore-TempState 包在另一个函数里，它只在该函数作用域内存在，
# 外面调用会报「术语不被识别」（第一次写这个单测时就踩了）。
$ErrorActionPreference = 'Stop'
$SETTINGS = Join-Path $env:TEMP 'DSH-Launch-Console-settings.json'
$BAK = "$SETTINGS.bak.test"
$script:fail = 0

function Assert([string]$what, [bool]$cond) {
  if ($cond) { Write-Output ("  [ OK ] " + $what) } else { Write-Output ("  [FAIL] " + $what); $script:fail++ }
}

# ── 护栏（与各脚本顶层插入的完全一致）──────────────────────────
$TempSettings = $SETTINGS
$TempSettingsBak = $BAK
$script:HadUserSettings = Test-Path $TempSettings
if ($script:HadUserSettings) { Copy-Item $TempSettings $TempSettingsBak -Force }
function Restore-TempState {
  if ($script:HadUserSettings) { Move-Item $TempSettingsBak $TempSettings -Force -ErrorAction SilentlyContinue }
  else { Remove-Item $TempSettings -Force -ErrorAction SilentlyContinue }
}
# ────────────────────────────────────────────────────────────

Write-Output "=== 用例 1：用户已有设置 -> 跑完必须原样还原 ==="
Remove-Item $SETTINGS, $BAK -Force -ErrorAction SilentlyContinue
$userContent = '{"url":"http://127.0.0.1:3080","profile":"用户自己的","close_action":"tray","auto_open_browser":true}'
Set-Content -Encoding UTF8 $SETTINGS $userContent
$userHash = (Get-FileHash $SETTINGS).Hash
$script:HadUserSettings = Test-Path $TempSettings
if ($script:HadUserSettings) { Copy-Item $TempSettings $TempSettingsBak -Force }
Assert "用户设置已被备份" (Test-Path $BAK)
Set-Content -Encoding UTF8 $SETTINGS '{"url":"http://127.0.0.1:3199","profile":"测试","auto_open_browser":false}'
Restore-TempState
Assert "跑完文件存在" (Test-Path $SETTINGS)
Assert "内容与用户原文件逐字节一致" ((Get-FileHash $SETTINGS).Hash -eq $userHash)
Assert "备份已清掉" (-not (Test-Path $BAK))
Assert "没有把测试用的端口留下来" ((Get-Content $SETTINGS -Raw) -notmatch '3199')
Remove-Item $SETTINGS -Force -ErrorAction SilentlyContinue

Write-Output ""
Write-Output "=== 用例 2：用户没有设置 -> 跑完不能留下文件 ==="
Remove-Item $SETTINGS, $BAK -Force -ErrorAction SilentlyContinue
$script:HadUserSettings = Test-Path $TempSettings
if ($script:HadUserSettings) { Copy-Item $TempSettings $TempSettingsBak -Force }
Assert "未备份（本来就没有）" (-not (Test-Path $BAK))
Set-Content -Encoding UTF8 $SETTINGS '{"url":"http://127.0.0.1:3199","profile":"测试"}'
Restore-TempState
Assert "跑完不残留 settings.json" (-not (Test-Path $SETTINGS))
Assert "跑完不残留 .bak" (-not (Test-Path $BAK))

Write-Output ""
if ($script:fail -eq 0) { Write-Output "全部通过 ✅" } else { Write-Output ("失败 " + $script:fail + " 项 ❌"); exit 1 }
