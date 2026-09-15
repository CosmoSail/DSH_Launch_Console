# 像素级校验占位页：找出内容区的“墨水”像素并打印 ASCII 图（背景 #16181d）
param([string]$Path = (Join-Path $PSScriptRoot 'loading-shot.png'))
$ErrorActionPreference = 'Stop'
# 路径约定：本脚本位于 <仓库根>\tests\，$ROOT 取其父目录即仓库根，$PSScriptRoot 即本目录
$ROOT = Split-Path $PSScriptRoot -Parent
$PROJ = $ROOT
Add-Type -AssemblyName System.Drawing
$bmp = [System.Drawing.Bitmap]::FromFile($Path)
$w = $bmp.Width; $h = $bmp.Height
Write-Output ("图片: {0}x{1}" -f $w, $h)

# 采样统计颜色
$colors = @{}
for ($y = 0; $y -lt $h; $y += 3) {
  for ($x = 0; $x -lt $w; $x += 3) {
    $c = $bmp.GetPixel($x, $y)
    $k = "$($c.R),$($c.G),$($c.B)"
    $colors[$k] = [int]$colors[$k] + 1
  }
}
Write-Output "--- 采样最多的 8 种颜色 ---"
$colors.GetEnumerator() | Sort-Object Value -Descending | Select-Object -First 8 |
  ForEach-Object { Write-Output ("  {0}  x{1}" -f $_.Key, $_.Value) }

# 墨水判定：与背景色 (22,24,29) 差异 > 40
function Is-Ink($c) {
  return ([Math]::Abs($c.R - 22) -gt 40) -or ([Math]::Abs($c.G - 24) -gt 40) -or ([Math]::Abs($c.B - 29) -gt 40)
}
$minX = $w; $maxX = 0; $minY = $h; $maxY = 0; $ink = 0
for ($y = 45; $y -lt $h; $y++) {         # 跳过标题栏
  for ($x = 0; $x -lt $w; $x++) {
    $c = $bmp.GetPixel($x, $y)
    if (Is-Ink $c) {
      $ink++
      if ($x -lt $minX) { $minX = $x }; if ($x -gt $maxX) { $maxX = $x }
      if ($y -lt $minY) { $minY = $y }; if ($y -gt $maxY) { $maxY = $y }
    }
  }
}
Write-Output ("墨水像素: {0}  包围盒: x {1}..{2}, y {3}..{4}" -f $ink, $minX, $maxX, $minY, $maxY)

# ASCII 图（每格 4x4 像素）
$step = 4
for ($y = $minY; $y -le $maxY; $y += $step) {
  $line = ''
  for ($x = $minX; $x -le $maxX; $x += $step) {
    $hit = $false
    for ($dy = 0; $dy -lt $step -and -not $hit; $dy++) {
      for ($dx = 0; $dx -lt $step -and -not $hit; $dx++) {
        if ($x + $dx -lt $w -and $y + $dy -lt $h -and (Is-Ink $bmp.GetPixel($x + $dx, $y + $dy))) { $hit = $true }
      }
    }
    $line += if ($hit) { '#' } else { '.' }
  }
  Write-Output $line
}
$bmp.Dispose()
