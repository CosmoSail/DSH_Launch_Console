@echo off
chcp 65001 >nul
cd /d "%~dp0"
if not exist "%~dp0DSH_Launch_Console.exe" (
  echo 未找到 DSH_Launch_Console.exe。请先双击 安装.bat 完成编译安装，再把安装目录里的 DSH_Launch_Console.exe 复制回本文件夹，然后重新运行 package.bat。
  pause
  exit /b 1
)
:: 版本号唯一来源：Cargo.toml（不在这里手写，避免产物名与程序版本不一致）
:: delims 里带上空格：Cargo.toml 里是 version = "x.y.z"，否则值会带前导空格与引号
for /f "usebackq tokens=2 delims== " %%v in (`findstr /b /c:"version" Cargo.toml`) do set "APPVER=%%~v"
if not defined APPVER (
  echo 无法从 Cargo.toml 解析版本号。
  pause
  exit /b 1
)
echo [1/3] 正在编译 Inno Setup 安装向导（需要包文件夹根目录已存在 DSH_Launch_Console.exe）...
set "ISCC="
where iscc >nul 2>nul && set "ISCC=iscc"
if not defined ISCC (
  if exist "%~dp0..\..\.innosetup\ISCC.exe" (
    set "ISCC=%~dp0..\..\.innosetup\ISCC.exe"
  ) else if exist "%~dp0..\.innosetup\ISCC.exe" (
    set "ISCC=%~dp0..\.innosetup\ISCC.exe"
  ) else (
    echo 未找到 ISCC.exe。请将 Inno Setup 便携版解压到 ..\..\.innosetup\ 或 ..\.innosetup\
    pause
    exit /b 1
  )
)
"%ISCC%" /DMyAppVersion=%APPVER% installer.iss
if errorlevel 1 (
  echo Inno Setup 打包失败，请检查上方错误信息。
) else (
  echo 完成：Output\DSH_Launch_Console-Setup-%APPVER%.exe
)
echo.
echo [2/3] 正在生成 Windows 源码压缩包（不含编译产物）...
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0make-archives.ps1" -Only windows
if errorlevel 1 (
  echo 压缩包生成失败。
) else (
  echo 完成：Output\DSH_Launch_Console-%APPVER%-windows-src.zip
)
echo.
echo [3/3] 正在生成 Linux 源码压缩包（不含编译产物）...
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0make-archives.ps1" -Only linux
if errorlevel 1 (
  echo 压缩包生成失败。
) else (
  echo 完成：Output\DSH_Launch_Console-%APPVER%-linux.tar.gz
)
pause