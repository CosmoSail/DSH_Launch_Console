# 名称显示核验 v2：重点抓「托盘提示」与「托盘菜单文案」
# 安全：只用改名副本 dsh-lab-name.exe + 独立互斥体，绝不碰用户实例。
$ErrorActionPreference = 'Stop'
$ROOT = Split-Path $PSScriptRoot -Parent
$PROJ = $ROOT
$EXE = "$PROJ\target\release\dsh-launch-console.exe"
$LABEXE = "$PSScriptRoot\dsh-lab-name.exe"
$NODE = 'C:\Program Files\nodejs\node.exe'
$SYS = "C:\Windows\system32;C:\Windows"
$PORT = 3198

Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
Add-Type @'
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public class N5 {
  delegate bool EnumProc(IntPtr h, IntPtr p);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr p);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetClassNameW(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint msg, IntPtr wp, IntPtr lp);
  public static List<string> MenuWindows() {
    var res = new List<string>();
    EnumWindows((h, p) => {
      var cls = new StringBuilder(64); GetClassNameW(h, cls, 64);
      if (cls.ToString() == "#32768") {
        uint pid; GetWindowThreadProcessId(h, out pid);
        var sb = new StringBuilder(256); GetWindowTextW(h, sb, 256);
        res.Add(h.ToInt64() + "|" + cls + "|" + sb);
      }
      return true;
    }, IntPtr.Zero);
    return res;
  }
}
'@

$live = Get-Process -Name DSH_Launch_Console -ErrorAction SilentlyContinue
if ($live) { Write-Output ("注意：已有实例在运行（" + ($live.Id -join ',') + "），本脚本用独立互斥体并存。") }
Get-Process -Name 'dsh-lab*' -ErrorAction SilentlyContinue | Stop-Process -Force
Get-NetTCPConnection -LocalPort $PORT -State Listen -ErrorAction SilentlyContinue |
  ForEach-Object { Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue }
Copy-Item $EXE $LABEXE -Force

$T = Join-Path $env:TEMP 'lab-name2'
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
const http = require('http');
const token = 'NAMETOKEN123456';
const args = process.argv.slice(2);
const pi = args.indexOf('--port');
const port = pi >= 0 ? parseInt(args[pi + 1], 10) : 3198;
setTimeout(() => console.log('dsh web: http://127.0.0.1:' + port + '/?token=' + token), 150);
http.createServer((q, s) => {
  if (q.url.indexOf('token=' + token) >= 0) { s.writeHead(200, {'Content-Type':'text/html; charset=utf-8'}); s.end('<!doctype html><title>LAB</title>lab'); }
  else { s.writeHead(401); s.end('no'); }
}).listen(port, '127.0.0.1');
'@ | Set-Content -Encoding ASCII (Join-Path $lib 'bin.js')

$env:DSH_LAUNCH_CONSOLE_URL = "http://127.0.0.1:$PORT"
$env:DSH_LAUNCH_CONSOLE_STATEDIR = "$T\state"
$env:DSH_LAUNCH_CONSOLE_NOMSGBOX = '1'
$env:DSH_LAUNCH_CONSOLE_MUTEX = 'DSH_Launch_Console_LabName_Mutex'
$env:PATH = "$T;$SYS"
Remove-Item Env:DSH_LAUNCH_CONSOLE_NPX -ErrorAction SilentlyContinue

$p = Start-Process -FilePath $LABEXE -PassThru
Start-Sleep -Seconds 4
$root = [System.Windows.Automation.AutomationElement]::RootElement

Write-Output "=== A) 托盘提示（展开 Win11 托盘溢出区后读取图标名）==="
$chevron = $null
foreach ($e in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)) {
  try {
    if ($e.Current.ControlType -eq [System.Windows.Automation.ControlType]::Button -and
        ($e.Current.Name -match 'Notification Chevron|通知 V 形|通知.*箭头|显示隐藏的图标')) { $chevron = $e; break }
  } catch { }
}
if ($chevron) {
  Write-Output ("  找到溢出区按钮: [" + $chevron.Current.Name + "]")
  try {
    $chevron.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    Start-Sleep -Milliseconds 1200
    foreach ($e in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)) {
      try {
        $n = $e.Current.Name
        if ($n -and $n -match 'DSH') { Write-Output ("  托盘区元素: [" + $n + "]  type=" + $e.Current.ControlType.ProgrammaticName) }
      } catch { }
    }
    $chevron.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()  # 收起
  } catch { Write-Output ("  溢出区操作失败: " + $_.Exception.Message) }
} else {
  Write-Output "  未找到 Win11 托盘溢出区按钮（可能托盘图标直接可见或系统版本不同）"
  foreach ($e in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)) {
    try { $n = $e.Current.Name; if ($n -and $n -match 'DSH') { Write-Output ("  可见 DSH 元素: [" + $n + "]") } } catch { }
  }
}

Write-Output "=== B) 托盘右键菜单文案（弹出真实菜单后用 UIA 逐项读取）==="
$trayHwnd = (Get-Content "$T\state\tray-hwnd" -ErrorAction SilentlyContinue)
if ($trayHwnd) {
  $h = [IntPtr][int]$trayHwnd.Trim()
  [N5]::PostMessageW($h, 0x8001, [IntPtr]1, [IntPtr]0x0205) | Out-Null   # WM_APP+1 回调 + WM_RBUTTONUP
  Start-Sleep -Seconds 2
  $menus = [N5]::MenuWindows()
  if ($menus.Count -eq 0) { Write-Output "  未发现 #32768 菜单窗口（菜单未弹出）" }
  foreach ($m in $menus) {
    $mh = [IntPtr][long]($m -split '\|')[0]
    Write-Output ("  菜单窗口 hwnd=" + $mh)
    try {
      $el = [System.Windows.Automation.AutomationElement]::FromHandle($mh)
      $items = $el.FindAll([System.Windows.Automation.TreeScope]::Descendants,
        (New-Object System.Windows.Automation.PropertyCondition(
          [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
          [System.Windows.Automation.ControlType]::MenuItem)))
      foreach ($it in $items) { Write-Output ("    菜单项: [" + $it.Current.Name + "]") }
      # 展开「设置」子菜单，读取其中的两项文案
      foreach ($it in $items) {
        if ($it.Current.Name -eq '设置') {
          try {
            $it.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
            Start-Sleep -Milliseconds 900
            $sub = $el.FindAll([System.Windows.Automation.TreeScope]::Descendants,
              (New-Object System.Windows.Automation.PropertyCondition(
                [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
                [System.Windows.Automation.ControlType]::MenuItem)))
            foreach ($s2 in $sub) { Write-Output ("    子菜单项: [" + $s2.Current.Name + "]  check=" + $s2.GetCurrentPropertyValue([System.Windows.Automation.TogglePattern]::ToggleStateProperty) ) }
            $it.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Collapse()
          } catch { Write-Output ("    展开子菜单失败: " + $_.Exception.Message) }
        }
      }
    } catch { Write-Output ("    UIA 读取菜单失败: " + $_.Exception.Message) }
  }
  Add-Type @'
using System;
using System.Runtime.InteropServices;
public class K2 { [DllImport("user32.dll")] public static extern void keybd_event(byte b, byte s, uint f, IntPtr e);
  public static void Esc() { keybd_event(0x1B,0,0,IntPtr.Zero); keybd_event(0x1B,0,2,IntPtr.Zero); } }
'@
  [K2]::Esc()
  Start-Sleep -Milliseconds 600
} else { Write-Output "  未找到 tray-hwnd" }

Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 800
Remove-Item $LABEXE -Force -ErrorAction SilentlyContinue
Get-NetTCPConnection -LocalPort $PORT -State Listen -ErrorAction SilentlyContinue |
  ForEach-Object { Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue }
Remove-Item -Recurse -Force $T -ErrorAction SilentlyContinue
