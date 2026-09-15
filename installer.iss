; DSH_Launch_Console — Inno Setup 安装脚本
; 按用户安装（无需管理员），开始菜单快捷方式 + 可选桌面快捷方式 + 卸载器
#define MyAppName "DSH Launch Console"
#define MyAppVersion "0.1.0"
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
OutputBaseFilename=DSH_Launch_Console-Setup-0.1.0
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
Source: "installer.iss"; DestDir: "{app}"; Flags: ignoreversion
Source: "package.bat"; DestDir: "{app}"; Flags: ignoreversion

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
           '本应用通过 npx 启动 DSH，请先安装 Node.js（https://nodejs.org），' + #13#10 +
           '否则应用可能无法启动 DSH。', mbInformation, MB_OK);
end;
