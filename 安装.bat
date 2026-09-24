@echo off
chcp 65001 >nul
setlocal EnableDelayedExpansion
cd /d "%~dp0"
title DSH_Launch_Console 一键安装

:: ============ 第一步：指定安装路径 ============
set "DEFDIR=%LOCALAPPDATA%\Programs\DSH_Launch_Console"
set "INSTDIR=%~1"
if "%INSTDIR%"=="" (
  if "%~2"=="" (
    echo 正在打开安装目录选择框，请选择安装位置...
    if exist "%TEMP%\dsh-pickdir.bat" del "%TEMP%\dsh-pickdir.bat"
    powershell -NoProfile -STA -ExecutionPolicy Bypass -EncodedCommand QQBkAGQALQBUAHkAcABlACAALQBBAHMAcwBlAG0AYgBsAHkATgBhAG0AZQAgAFMAeQBzAHQAZQBtAC4AVwBpAG4AZABvAHcAcwAuAEYAbwByAG0AcwAKACQAZAAgAD0AIABOAGUAdwAtAE8AYgBqAGUAYwB0ACAAUwB5AHMAdABlAG0ALgBXAGkAbgBkAG8AdwBzAC4ARgBvAHIAbQBzAC4ARgBvAGwAZABlAHIAQgByAG8AdwBzAGUAcgBEAGkAYQBsAG8AZwAKACQAZAAuAEQAZQBzAGMAcgBpAHAAdABpAG8AbgAgAD0AIAAnAAmQ6WIgAEQAUwBIACAATABhAHUAbgBjAGgAIABDAG8AbgBzAG8AbABlACAAhHaJW8WIh2X2TjlZJwAKACQAZAAuAFMAZQBsAGUAYwB0AGUAZABQAGEAdABoACAAPQAgACIAJABlAG4AdgA6AEwATwBDAEEATABBAFAAUABEAEEAVABBAFwAUAByAG8AZwByAGEAbQBzAFwARABTAEgAXwBMAGEAdQBuAGMAaABfAEMAbwBuAHMAbwBsAGUAIgAKACQAZAAuAFMAaABvAHcATgBlAHcARgBvAGwAZABlAHIAQgB1AHQAdABvAG4AIAA9ACAAJAB0AHIAdQBlAAoAJABuAHUAbABsACAAPQAgACgATgBlAHcALQBPAGIAagBlAGMAdAAgAC0AQwBvAG0ATwBiAGoAZQBjAHQAIABXAFMAYwByAGkAcAB0AC4AUwBoAGUAbABsACkALgBBAHAAcABBAGMAdABpAHYAYQB0AGUAKAAkAFAASQBEACkACgBpAGYAIAAoACQAZAAuAFMAaABvAHcARABpAGEAbABvAGcAKAApACAALQBlAHEAIABbAFMAeQBzAHQAZQBtAC4AVwBpAG4AZABvAHcAcwAuAEYAbwByAG0AcwAuAEQAaQBhAGwAbwBnAFIAZQBzAHUAbAB0AF0AOgA6AE8ASwApACAAewAKACAAIAAkAGwAaQBuAGUAIAA9ACAAJwBzAGUAdAAgACIASQBOAFMAVABEAEkAUgA9ACcAIAArACAAJABkAC4AUwBlAGwAZQBjAHQAZQBkAFAAYQB0AGgAIAArACAAJwAiACcACgAgACAAWwBJAE8ALgBGAGkAbABlAF0AOgA6AFcAcgBpAHQAZQBBAGwAbABUAGUAeAB0ACgAIgAkAGUAbgB2ADoAVABFAE0AUABcAGQAcwBoAC0AcABpAGMAawBkAGkAcgAuAGIAYQB0ACIALAAgACQAbABpAG4AZQAsACAAKABOAGUAdwAtAE8AYgBqAGUAYwB0ACAAVABlAHgAdAAuAFUAVABGADgARQBuAGMAbwBkAGkAbgBnACgAJABmAGEAbABzAGUAKQApACkACgB9AA==
    if exist "%TEMP%\dsh-pickdir.bat" (
      call "%TEMP%\dsh-pickdir.bat"
      del "%TEMP%\dsh-pickdir.bat"
    )
  )
)
if "!INSTDIR!"=="" (
  if "%~2"=="" (
    echo 未选择目录。请手动输入安装路径（直接回车使用默认：!DEFDIR!；输入 q 取消）
    set /p "INSTDIR=安装路径："
  )
)
if /i "!INSTDIR!"=="q" (
  echo 已取消安装。
  pause
  exit /b 0
)
if "!INSTDIR!"=="" set "INSTDIR=%DEFDIR%"
echo.
echo 安装位置：!INSTDIR!
if not exist "!INSTDIR!" mkdir "!INSTDIR!"

:: ============ 第二步：编译，产物直接放进安装目录 ============
echo 开始编译，构建产物直接写入安装目录（需要 Rust 工具链，首次约几分钟）...
cargo build --release --target-dir "!INSTDIR!\target"
if errorlevel 1 (
  echo 编译失败，请检查上方错误信息。
  pause
  exit /b 1
)
copy /Y "!INSTDIR!\target\release\dsh-launch-console.exe" "!INSTDIR!\DSH_Launch_Console.exe" >nul || (
  echo 复制程序文件失败，请检查目标路径权限后重试。
  pause
  exit /b 1
)
copy /Y "icon.ico" "!INSTDIR!\" >nul || echo 警告：图标复制失败。
copy /Y "README.md" "!INSTDIR!\" >nul
echo 程序已生成到安装目录（黑色鲸鱼图标已由编译期嵌入）。

:: 在安装目录现场生成卸载执行文件
> "!INSTDIR!\uninstall.bat" echo @echo off
>> "!INSTDIR!\uninstall.bat" echo setlocal
>> "!INSTDIR!\uninstall.bat" echo title 卸载 DSH_Launch_Console
>> "!INSTDIR!\uninstall.bat" echo echo 正在卸载 DSH_Launch_Console ...
>> "!INSTDIR!\uninstall.bat" echo del /F /Q "%%APPDATA%%\Microsoft\Windows\Start Menu\Programs\DSH_Launch_Console.lnk" ^>nul 2^>^&1
>> "!INSTDIR!\uninstall.bat" echo del /F /Q "%%USERPROFILE%%\Desktop\DSH_Launch_Console.lnk" ^>nul 2^>^&1
>> "!INSTDIR!\uninstall.bat" echo del /F /Q "%%PUBLIC%%\Desktop\DSH_Launch_Console.lnk" ^>nul 2^>^&1
>> "!INSTDIR!\uninstall.bat" echo reg delete "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\DSH_Launch_Console" /f ^>nul 2^>^&1
>> "!INSTDIR!\uninstall.bat" echo cd /d "%%USERPROFILE%%"
>> "!INSTDIR!\uninstall.bat" echo ping -n 2 127.0.0.1 ^>nul
>> "!INSTDIR!\uninstall.bat" echo rmdir /S /Q "!INSTDIR!" ^>nul 2^>^&1
>> "!INSTDIR!\uninstall.bat" echo echo 卸载完成。
>> "!INSTDIR!\uninstall.bat" echo if /i not "%%1"=="/S" pause


if "%~2"=="" (
  :: 开始菜单快捷方式
  powershell -NoProfile -ExecutionPolicy Bypass -Command "=(New-Object -ComObject WScript.Shell).CreateShortcut([Environment]::GetFolderPath('Programs')+'\DSH_Launch_Console.lnk');.TargetPath='!INSTDIR!\DSH_Launch_Console.exe';.IconLocation='!INSTDIR!\DSH_Launch_Console.exe';.WorkingDirectory='!INSTDIR!';.Save()" >nul 2>&1

  :: 桌面快捷方式
  set /p "DESK=创建桌面快捷方式？(Y/N，默认 Y)："
  if /i not "!DESK!"=="N" (
    powershell -NoProfile -ExecutionPolicy Bypass -Command "=(New-Object -ComObject WScript.Shell).CreateShortcut([Environment]::GetFolderPath('Desktop')+'\DSH_Launch_Console.lnk');.TargetPath='!INSTDIR!\DSH_Launch_Console.exe';.IconLocation='!INSTDIR!\DSH_Launch_Console.exe';.WorkingDirectory='!INSTDIR!';.Save()" >nul 2>&1
  )
)

:: 控制面板卸载条目（HKCU，无需管理员权限）
:: 版本号从 Cargo.toml 读，不在脚本里手写（避免与程序实际版本不一致）
:: delims 里带上空格：Cargo.toml 里是 version = "x.y.z"，否则值会带前导空格与引号
for /f "usebackq tokens=2 delims== " %%v in (`findstr /b /c:"version" Cargo.toml`) do set "APPVER=%%~v"
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\DSH_Launch_Console" /v DisplayName /d "DSH_Launch_Console" /f >nul
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\DSH_Launch_Console" /v DisplayVersion /d "!APPVER!" /f >nul
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\DSH_Launch_Console" /v Publisher /d "DSH" /f >nul
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\DSH_Launch_Console" /v InstallLocation /d "!INSTDIR!" /f >nul
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\DSH_Launch_Console" /v DisplayIcon /d "!INSTDIR!\DSH_Launch_Console.exe" /f >nul
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\DSH_Launch_Console" /v UninstallString /d "\"!INSTDIR!\uninstall.bat\"" /f >nul
reg add "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\DSH_Launch_Console" /v QuietUninstallString /d "\"!INSTDIR!\uninstall.bat\" /S" /f >nul

echo.
echo 安装完成！
echo   位置：!INSTDIR!
if "%~2"=="" (
  echo   开始菜单已创建快捷方式。
  set /p "RUN=是否立即运行？(Y/N，默认 Y)："
  if /i not "!RUN!"=="N" start "" "!INSTDIR!\DSH_Launch_Console.exe"
  pause
)
exit /b 0