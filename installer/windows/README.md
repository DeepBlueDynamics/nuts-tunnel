# Windows installer for nuts-client

Inno Setup script that wraps the release binary in a one-click `.exe`
installer with system-PATH integration.

## Build locally

### 1. Build the binary

From the **`nuts-tunnel/` repo root** (not this directory):

```powershell
cargo build --release --target x86_64-pc-windows-msvc -p nuts-client
```

Output: `target/x86_64-pc-windows-msvc/release/nuts-client.exe`

If you don't have the MSVC target yet:

```powershell
rustup target add x86_64-pc-windows-msvc
```

### 2. Install Inno Setup (one time)

Download Inno Setup 6 from <https://jrsoftware.org/isdl.php> and install
with defaults — `iscc.exe` will end up at
`C:\Program Files (x86)\Inno Setup 6\iscc.exe`.

Or with Chocolatey:

```powershell
choco install innosetup -y
```

### 3. Compile the installer

From this directory:

```powershell
& "C:\Program Files (x86)\Inno Setup 6\iscc.exe" installer.iss
```

Override the version at compile time:

```powershell
& "C:\Program Files (x86)\Inno Setup 6\iscc.exe" /DMyAppVersion=0.2.0 installer.iss
```

Output: `Output\nuts-client-setup-<version>.exe` (next to this README).

## What the installer does

| Step | Where |
|---|---|
| Copies `nuts-client.exe` | `%ProgramFiles%\Nuts Tunnel\` |
| Adds install dir to **system PATH** | `HKLM\…\Environment\Path` (broadcast on completion) |
| Drops `config.example.toml` | `%APPDATA%\nuts-tunnel\` (only if missing) |
| Drops `post-install.txt` | `%ProgramFiles%\Nuts Tunnel\` |
| Start Menu shortcut | "Nuts Tunnel — Open Console" → opens a `cmd` window |
| Uninstaller | Add/Remove Programs → "Nuts Tunnel" |

Removes itself cleanly on uninstall, including taking the install dir back
out of `PATH`. **Does not** touch your `%APPDATA%\nuts-tunnel\` configs.

## Code signing — not done yet

The output `.exe` is **unsigned**. Users will see SmartScreen ("Windows
protected your PC") on first run and need to click *More info → Run anyway*.

To sign it (e.g. via Azure Trusted Signing) wrap the `iscc` step with a
`signtool sign` call. Drop me a sign command and I'll wire it into the GHA
workflow.

## Releasing

Push a tag matching `v*` to the GitHub repo and the
[`release-windows.yml`](../../.github/workflows/release-windows.yml)
workflow runs end-to-end: builds the Rust binary, compiles the installer,
uploads it as a release asset.

```bash
git tag v0.1.0
git push origin v0.1.0
```

GitHub Release will have `nuts-client-setup-0.1.0.exe` attached.
