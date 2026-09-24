; DSH_Launch_Console — Inno Setup 安装脚本
; 按用户安装（无需管理员），开始菜单快捷方式 + 可选桌面快捷方式 + 卸载器
;
; 版本号：**不在这里手写**。Cargo.toml 是唯一来源，打包脚本读出来用
;   ISCC.exe /DMyAppVersion=<版本> installer.iss
; 传进来。没传时（有人直接跑 ISCC）回落到读 exe 的版本信息——那份也是
; build.rs 从 Cargo.toml 注入的，所以两条路都指向同一个来源。
; 回退值是四段式（x.y.z.0），去掉末尾的 .0 才是发布用的三段式。
#define MyAppName "DSH Launch Console"
#ifndef MyAppVersion
  #define MyAppVersion GetVersionNumbersString(AddBackslash(SourcePath) + "DSH_Launch_Console.exe")
  #if Len(MyAppVersion) > 2
    #if Copy(MyAppVersion, Len(MyAppVersion) - 1, 2) == ".0"
      #define MyAppVersion Copy(MyAppVersion, 1, Len(MyAppVersion) - 2)
    #endif
  #endif
#endif
#define MyAppExeName "DSH_Launch_Console.exe"

[Setup]
AppId={{4764FC99-FEE0-48B7-8B83-115578E612DE}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher=DSH
DefaultDirName={localappdata}\Programs\DSH_Launch_Console
DefaultGroupName={#MyAppName}
PrivilegesRequired=lowest
OutputDir=Output
OutputBaseFilename=DSH_Launch_Console-Setup-{#MyAppVersion}
SetupIconFile=icon.ico
UninstallDisplayIcon={app}\icon.ico
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
DisableProgramGroupPage=yes
ShowLanguageDialog=no

[Languages]
Name: "chinesesimplified"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "DSH_Launch_Console.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "icon.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "README.md"; DestDir: "{app}"; Flags: ignoreversion
; README 里的截图，缺了会变成坏链
Source: "docs\*.png"; DestDir: "{app}\docs"; Flags: ignoreversion
Source: "installer.iss"; DestDir: "{app}"; Flags: ignoreversion
Source: "package.bat"; DestDir: "{app}"; Flags: ignoreversion
; package.bat 的源码包步骤调用它，缺了第 2、3 步会失败
Source: "make-archives.ps1"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; IconFilename: "{app}\icon.ico"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon; IconFilename: "{app}\icon.ico"

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#MyAppName}}"; Flags: nowait postinstall skipifsilent

[Code]
function InitializeSetup(): Boolean;
var
  ResultCode: Integer;
begin
  Result := True;
  // 通过 PATH 探测 node（覆盖官方安装包、nvm、scoop、winget 等所有安装方式）
  if not (Exec('cmd.exe', '/C where node', '', SW_HIDE, ewWaitUntilTerminated, ResultCode) and
          (ResultCode = 0)) then
    MsgBox('未检测到 Node.js。' + #13#10 +
           '本应用用 node 直接启动 DeepSeek Harness（不走 npx），' + #13#10 +
           '请先安装 Node.js（https://nodejs.org，建议 18 及以上），' + #13#10 +
           '否则无法启动 DeepSeek Harness。', mbInformation, MB_OK);
end;
