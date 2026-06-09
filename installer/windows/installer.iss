; Inno Setup script for nuts-client (Windows installer)
;
; Build:
;   1. Build the binary first:
;        cargo build --release --target x86_64-pc-windows-msvc -p nuts-client
;   2. Compile this script:
;        "C:\Program Files (x86)\Inno Setup 6\iscc.exe" installer.iss
;
; Override version at compile time:
;   iscc /DMyAppVersion=0.2.0 installer.iss

#ifndef MyAppVersion
  #define MyAppVersion "0.1.0"
#endif
#define MyAppName        "Nuts Tunnel"
#define MyAppPublisher   "Deep Blue Dynamics"
#define MyAppURL         "https://tunnel.nuts.services"
#define MyAppExeName     "nuts-client.exe"
#define MyBinarySource   "..\..\target\x86_64-pc-windows-msvc\release\nuts-client.exe"

[Setup]
AppId={{8E0D2C7A-7C2E-4F1F-9AE2-6C3D0DAB7711}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}
DefaultDirName={autopf}\Nuts Tunnel
DefaultGroupName=Nuts Tunnel
DisableProgramGroupPage=yes
LicenseFile=
InfoAfterFile=post-install.txt
OutputBaseFilename=nuts-client-setup-{#MyAppVersion}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=admin
ArchitecturesInstallIn64BitMode=x64compatible
ArchitecturesAllowed=x64compatible
ChangesEnvironment=yes
UninstallDisplayIcon={app}\{#MyAppExeName}
SetupIconFile=

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "addtopath"; Description: "Add nuts-client to system PATH (recommended)"; GroupDescription: "Integration:"; Flags: checkedonce
Name: "dropconfig"; Description: "Drop a sample config at %APPDATA%\nuts-tunnel\config.example.toml"; GroupDescription: "Integration:"; Flags: checkedonce

[Files]
Source: "{#MyBinarySource}"; DestDir: "{app}"; Flags: ignoreversion
Source: "post-install.txt";  DestDir: "{app}"; Flags: ignoreversion
Source: "config.example.toml"; DestDir: "{userappdata}\nuts-tunnel"; \
    DestName: "config.example.toml"; Flags: onlyifdoesntexist uninsneveruninstall; Tasks: dropconfig

[Icons]
Name: "{group}\Nuts Tunnel — Open Console"; Filename: "{cmd}"; \
    Parameters: "/k echo Nuts Tunnel & echo. & echo Try:   nuts-client --help & echo."; \
    WorkingDir: "{userappdata}\nuts-tunnel"; Comment: "Terminal with nuts-client on PATH"
Name: "{group}\Nuts Tunnel Website"; Filename: "{#MyAppURL}"

[Run]
Filename: "{cmd}"; Parameters: "/k echo Nuts Tunnel installed. & echo. & {#MyAppExeName} --help & echo. & echo (Press any key to close.) & pause >nul"; \
    Description: "Open a terminal and show --help"; Flags: postinstall skipifsilent unchecked

[Code]
const
  EnvKey       = 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment';
  ModPathName  = 'NutsTunnel.Path';

function IsPathInList(const NewItem, PathList: string): Boolean;
var
  S, Item: string;
  P: Integer;
begin
  Result := False;
  S := ';' + Lowercase(PathList) + ';';
  Item := ';' + Lowercase(NewItem) + ';';
  if Pos(Item, S) > 0 then begin
    Result := True;
    exit;
  end;
  // also tolerate a trailing backslash
  Item := ';' + Lowercase(NewItem) + '\;';
  P := Pos(Item, S);
  if P > 0 then Result := True;
end;

procedure AddToSystemPath(const NewItem: string);
var
  Existing: string;
begin
  if not RegQueryStringValue(HKEY_LOCAL_MACHINE, EnvKey, 'Path', Existing) then
    Existing := '';
  if IsPathInList(NewItem, Existing) then exit;
  if (Length(Existing) > 0) and (Existing[Length(Existing)] <> ';') then
    Existing := Existing + ';';
  Existing := Existing + NewItem;
  RegWriteExpandStringValue(HKEY_LOCAL_MACHINE, EnvKey, 'Path', Existing);
end;

procedure RemoveFromSystemPath(const Item: string);
var
  Existing, Lower, ItemLower, Rebuilt, Part: string;
  Pieces: TArrayOfString;
  i: Integer;
begin
  if not RegQueryStringValue(HKEY_LOCAL_MACHINE, EnvKey, 'Path', Existing) then exit;
  Lower := Lowercase(Existing);
  ItemLower := Lowercase(Item);
  if Pos(ItemLower, Lower) = 0 then exit;
  // split on ';' and rebuild
  SetArrayLength(Pieces, 0);
  while Length(Existing) > 0 do begin
    i := Pos(';', Existing);
    if i = 0 then begin
      Part := Existing;
      Existing := '';
    end else begin
      Part := Copy(Existing, 1, i - 1);
      Existing := Copy(Existing, i + 1, Length(Existing) - i);
    end;
    if (Length(Part) > 0) and (Lowercase(Part) <> ItemLower) and
       (Lowercase(Part) <> ItemLower + '\') then
    begin
      if Length(Rebuilt) > 0 then Rebuilt := Rebuilt + ';';
      Rebuilt := Rebuilt + Part;
    end;
  end;
  RegWriteExpandStringValue(HKEY_LOCAL_MACHINE, EnvKey, 'Path', Rebuilt);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then begin
    if WizardIsTaskSelected('addtopath') then
      AddToSystemPath(ExpandConstant('{app}'));
  end;
end;

procedure CurUninstallStepChanged(CurStep: TUninstallStep);
begin
  if CurStep = usPostUninstall then
    RemoveFromSystemPath(ExpandConstant('{app}'));
end;
