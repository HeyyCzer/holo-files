; holo installer (Inno Setup 6). Build with installer\build.ps1, which compiles the
; release binaries and passes AppVersion / BinDir / IconFile.
;
; Per-user install, no admin: %LOCALAPPDATA%\Programs\holo, `holo` on the user PATH,
; "holo" in the Start menu. Upgrades stop the daemon and the tray first and re-point the
; logon task at the new holo.exe. Uninstall keeps sync roots and data unless asked.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef BinDir
  #define BinDir "..\target\release"
#endif

#define TaskName "HoloFiles"

[Setup]
AppId={{B31B6BE9-5ED5-4619-8672-5ADB8AB099F1}
AppName=holo
AppVersion={#AppVersion}
AppVerName=holo {#AppVersion}
AppPublisher=Rafael
DefaultDirName={localappdata}\Programs\holo
DisableDirPage=yes
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\target\installer
OutputBaseFilename=holo-setup-{#AppVersion}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ChangesEnvironment=yes
CloseApplications=no
UninstallDisplayName=holo
UninstallDisplayIcon={app}\holo-ui.exe
ShowLanguageDialog=no
#ifdef IconFile
SetupIconFile={#IconFile}
#endif

[Languages]
Name: "ptbr"; MessagesFile: "compiler:Languages\BrazilianPortuguese.isl"

[Tasks]
Name: "path"; Description: "Adicionar o comando &holo ao terminal (PATH do usuário)"
Name: "autostart"; Description: "Mostrar o ícone na &bandeja ao entrar no Windows"

[Files]
Source: "{#BinDir}\holo.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\holo-ui.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\holo"; Filename: "{app}\holo-ui.exe"; Comment: "Status e controle do holo"

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; \
  ValueName: "holo-ui"; ValueData: """{app}\holo-ui.exe"" --tray"; Tasks: autostart

[Run]
; The logon task stores holo.exe's path: re-register it at the new location. The daemon
; comes back only if it was running before the upgrade.
Filename: "{app}\holo.exe"; Parameters: "service install"; Flags: runhidden; \
  StatusMsg: "Atualizando o serviço do holo..."; Check: TaskWasInstalled and DaemonWasRunning
Filename: "{app}\holo.exe"; Parameters: "service install --no-start"; Flags: runhidden; \
  StatusMsg: "Atualizando o serviço do holo..."; Check: TaskWasInstalled and not DaemonWasRunning
Filename: "{app}\holo.exe"; Parameters: "run"; Flags: runhidden nowait; \
  Check: DaemonWasRunning and not TaskWasInstalled
Filename: "{app}\holo-ui.exe"; Parameters: "--tray"; Flags: nowait; Check: TrayWasRunning
Filename: "{app}\holo-ui.exe"; Description: "Abrir o holo"; Flags: postinstall nowait skipifsilent

[Code]
var
  TaskInstalled, TrayRunning, DaemonRunning: Boolean;

function Run(const Exe, Params: String): Integer;
begin
  if not Exec(Exe, Params, '', SW_HIDE, ewWaitUntilTerminated, Result) then
    Result := -1;
end;

function IsTaskInstalled(): Boolean;
begin
  Result := Run(ExpandConstant('{sys}\schtasks.exe'), '/Query /TN {#TaskName}') = 0;
end;

function IsTrayRunning(): Boolean;
begin
  Result := Run(ExpandConstant('{cmd}'),
    '/C tasklist /FI "IMAGENAME eq holo-ui.exe" /NH | find /I "holo-ui.exe"') = 0;
end;

function TaskWasInstalled(): Boolean;
begin
  Result := TaskInstalled;
end;

function DaemonWasRunning(): Boolean;
begin
  Result := DaemonRunning;
end;

function TrayWasRunning(): Boolean;
begin
  Result := TrayRunning;
end;

{ The daemon only leaves a lock file behind once it has run on this machine. }
function DaemonEverRan(): Boolean;
begin
  Result := FileExists(ExpandConstant('{localappdata}\holo\data\daemon.lock'));
end;

procedure KillTray();
begin
  Run(ExpandConstant('{sys}\taskkill.exe'), '/F /IM holo-ui.exe');
end;

{ Executables in use can't be replaced: stop the daemon cleanly and close the tray. }
function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  TaskInstalled := IsTaskInstalled();
  TrayRunning := IsTrayRunning();
  DaemonRunning := False;
  if DaemonEverRan() then
  begin
    ExtractTemporaryFile('holo.exe');
    DaemonRunning := Run(ExpandConstant('{tmp}\holo.exe'), 'service status --check') = 0;
    if DaemonRunning then
      Run(ExpandConstant('{tmp}\holo.exe'), 'service stop');
  end;
  KillTray();
  Result := '';
end;

{ ---------------------------------------------------------------- PATH }

function PathKey(): String;
begin
  Result := 'Environment';
end;

function InPath(const Path, Dir: String): Boolean;
begin
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Path) + ';') > 0;
end;

procedure AddToPath(const Dir: String);
var
  Path: String;
begin
  if not RegQueryStringValue(HKCU, PathKey(), 'Path', Path) then
    Path := '';
  if InPath(Path, Dir) then
    Exit;
  if (Path <> '') and (Copy(Path, Length(Path), 1) <> ';') then
    Path := Path + ';';
  RegWriteExpandStringValue(HKCU, PathKey(), 'Path', Path + Dir);
end;

procedure RemoveFromPath(const Dir: String);
var
  Path, Rest, Part, Kept: String;
  I: Integer;
begin
  if not RegQueryStringValue(HKCU, PathKey(), 'Path', Path) then
    Exit;
  Rest := Path + ';';
  Kept := '';
  while Rest <> '' do
  begin
    I := Pos(';', Rest);
    Part := Copy(Rest, 1, I - 1);
    Delete(Rest, 1, I);
    if (Part <> '') and (CompareText(Part, Dir) <> 0) then
    begin
      if Kept <> '' then
        Kept := Kept + ';';
      Kept := Kept + Part;
    end;
  end;
  if Kept <> Path then
    RegWriteExpandStringValue(HKCU, PathKey(), 'Path', Kept);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    if WizardIsTaskSelected('path') then
      AddToPath(ExpandConstant('{app}'))
    else
      RemoveFromPath(ExpandConstant('{app}'));
  end;
end;

{ ----------------------------------------------------------- uninstall }

var
  RemoveData: Boolean;

function InitializeUninstall(): Boolean;
begin
  RemoveData := False;
  if not UninstallSilent() then
    RemoveData := MsgBox(
      'Também remover as pastas sincronizadas do Explorer e apagar os dados do holo (login, banco, logs)?' + #13#10 + #13#10 +
      'Sim: arquivos que estão só na nuvem somem deste PC (continuam na nuvem); arquivos já baixados ficam como arquivos normais. ' +
      'Envios ainda na fila são perdidos.' + #13#10 + #13#10 +
      'Não: tudo fica como está e reinstalar o holo continua de onde parou.',
      mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES;
  Result := True;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Holo: String;
begin
  Holo := ExpandConstant('{app}\holo.exe');
  if CurUninstallStep = usUninstall then
  begin
    { Nothing of holo may keep running from files about to be deleted. }
    if FileExists(Holo) then
    begin
      Run(Holo, 'service uninstall');
      Run(Holo, 'service stop');
      if RemoveData then
        Run(Holo, 'remove-root --all --yes');
    end;
    KillTray();
    RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', 'holo-ui');
    RemoveFromPath(ExpandConstant('{app}'));
  end;
  if (CurUninstallStep = usPostUninstall) and RemoveData then
  begin
    DelTree(ExpandConstant('{localappdata}\holo'), True, True, True);
    DelTree(ExpandConstant('{userappdata}\holo'), True, True, True);
  end;
end;
