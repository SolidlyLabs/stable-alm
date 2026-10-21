; Inno Setup script: per-user Windows installer for the Rust app (rs/target/release/StableALM.exe).
;   cd rs && cargo build --release
;   "%LOCALAPPDATA%\Programs\Inno Setup 6\ISCC.exe" /DAppVersion=0.6.0 installer\StableALM.iss
; Output: rs\dist\StableALM-Setup-<version>.exe
; Program: %LOCALAPPDATA%\Programs\StableALM (no admin). State: %LOCALAPPDATA%\StableALM, kept on
; uninstall unless the user chooses to delete it. A running instance is stopped gracefully first
; (StableALM.exe --quit waits for the current cycle to finish).

#ifndef AppVersion
  #define AppVersion "0.6.0"
#endif
#ifndef ExePath
  #define ExePath "..\target\release\StableALM.exe"
#endif

[Setup]
AppId={{6C1F3B7E-5A0D-4C59-9E7B-6A2F1D3C8B41}
AppName=Stable ALM
AppVersion={#AppVersion}
AppVerName=Stable ALM {#AppVersion}
AppPublisher=Solidly Labs
AppPublisherURL=https://github.com/SolidlyLabs/stable-alm
DefaultDirName={localappdata}\Programs\StableALM
DefaultGroupName=Stable ALM
DisableProgramGroupPage=yes
DisableDirPage=yes
PrivilegesRequired=lowest
OutputDir=..\dist
OutputBaseFilename=StableALM-Setup-{#AppVersion}
SetupIconFile=alm.ico
UninstallDisplayIcon={app}\alm.ico
LicenseFile=..\..\LICENSE
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesInstallIn64BitMode=x64compatible
ArchitecturesAllowed=x64compatible
CloseApplications=no

[Tasks]
Name: "autostart"; Description: "Start Stable ALM when I sign in to Windows"; GroupDescription: "Startup:"
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Shortcuts:"; Flags: unchecked

[Files]
Source: "{#ExePath}"; DestDir: "{app}"; Flags: ignoreversion
Source: "alm.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\Stable ALM"; Filename: "{app}\StableALM.exe"; IconFilename: "{app}\alm.ico"
Name: "{userdesktop}\Stable ALM"; Filename: "{app}\StableALM.exe"; IconFilename: "{app}\alm.ico"; Tasks: desktopicon

[Registry]
; Same value the app's own "Start with Windows" menu item reads and writes.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "StableALM"; \
  ValueData: """{app}\StableALM.exe"""; Tasks: autostart; Flags: uninsdeletevalue

[Run]
Filename: "{app}\StableALM.exe"; Description: "Start Stable ALM now"; Flags: nowait postinstall skipifsilent
; an update the app installed itself (silent): start the new version
Filename: "{app}\StableALM.exe"; Flags: nowait; Check: WizardSilent

[UninstallRun]
Filename: "{app}\StableALM.exe"; Parameters: "--quit"; Flags: runhidden waituntilterminated; RunOnceId: "QuitApp"

[UninstallDelete]
Type: filesandordirs; Name: "{app}"

[Code]
var
  DeleteData: Boolean;

{ Stop a running instance gracefully (it finishes the current cycle) before files are replaced. }
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Code: Integer;
begin
  Result := '';
  if FileExists(ExpandConstant('{app}\StableALM.exe')) then
    Exec(ExpandConstant('{app}\StableALM.exe'), '--quit', '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

function InitializeUninstall(): Boolean;
begin
  DeleteData := SuppressibleMsgBox('Also delete your Stable ALM data (settings, history and the stored key) in ' +
    ExpandConstant('{localappdata}\StableALM') + '?' + #13#10#13#10 +
    'This does not withdraw your liquidity positions. Withdraw them first (set the allocations to 0) ' +
    'if you want the funds back in the wallet.', mbConfirmation, MB_YESNO or MB_DEFBUTTON2, IDNO) = IDYES;
  Result := True;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if (CurUninstallStep = usPostUninstall) and DeleteData then
    DelTree(ExpandConstant('{localappdata}\StableALM'), True, True, True);
end;
