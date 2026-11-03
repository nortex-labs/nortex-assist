; Inno Setup script for Nortex Assist.
; Build:  ISCC.exe installer\nortex-assist.iss        (after `cargo build --release`)
; Output: installer\Output\NortexAssist-Setup-<version>.exe

#define MyAppName "Nortex Assist"
#define MyAppVersion "1.3.1"
#define MyAppExe "nortex-assist.exe"

[Setup]
AppId={{B7C4F2A1-6E3D-4C59-9A0B-3F1D8E5A7C24}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher=Nortex
VersionInfoVersion={#MyAppVersion}
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
; installs for the current user by default (no admin rights needed); the wizard offers "for all users"
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
OutputDir=Output
OutputBaseFilename=NortexAssist-Setup-{#MyAppVersion}
SetupIconFile=..\icon.ico
UninstallDisplayIcon={app}\{#MyAppExe}
UninstallDisplayName={#MyAppName}
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

[Languages]
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\target\release\{#MyAppExe}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\{#MyAppExe}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExe}"; Tasks: desktopicon

[Registry]
; the app can add itself to Windows autostart from its settings; remove that entry on uninstall
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "NortexAssist"; Flags: uninsdeletevalue dontcreatekey

[Run]
Filename: "{app}\{#MyAppExe}"; Description: "{cm:LaunchProgram,{#MyAppName}}"; Flags: nowait postinstall skipifsilent
; an update from inside the app runs the installer silently: start the new version afterwards
Filename: "{app}\{#MyAppExe}"; Flags: nowait runasoriginaluser; Check: WizardSilent

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/im {#MyAppExe} /f"; Flags: runhidden; RunOnceId: "StopNortexAssist"

[Code]
// stop a running copy so its exe can be replaced
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/im {#MyAppExe} /f', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Result := '';
end;

// settings and history are kept on uninstall unless the user asks to remove them
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  DataDir: String;
begin
  if CurUninstallStep = usPostUninstall then
  begin
    DataDir := ExpandConstant('{userappdata}\NortexAssist');
    if DirExists(DataDir) and (not UninstallSilent) then
      if MsgBox('Удалить также настройки, ключ API и историю (' + DataDir + ')?', mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
        DelTree(DataDir, True, True, True);
  end;
end;
