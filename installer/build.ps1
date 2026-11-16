# Builds the release binaries and the installer: target\installer\holo-setup-<version>.exe
#   .\installer\build.ps1            # build + package
#   .\installer\build.ps1 -NoBuild   # package the binaries already in target\release
param([switch]$NoBuild)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Push-Location $root
try {
    if (-not $NoBuild) {
        cargo build --release -p holo-cli -p holo-ui
        if ($LASTEXITCODE) { throw "cargo build failed" }
    }

    $version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"' |
        Select-Object -First 1).Matches[0].Groups[1].Value

    # holo-ui's build.rs rasterizes assets/icons/holo.svg into its OUT_DIR.
    $icon = Get-ChildItem target\release\build\holo-ui-*\out\holo.ico -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1

    $iscc = @(
        (Get-Command iscc -ErrorAction SilentlyContinue).Source,
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
    ) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
    if (-not $iscc) { throw "Inno Setup 6 not found: winget install JRSoftware.InnoSetup" }

    $defines = @("/DAppVersion=$version", "/DBinDir=$root\target\release")
    if ($icon) { $defines += "/DIconFile=$($icon.FullName)" }
    & $iscc /Q @defines installer\holo.iss
    if ($LASTEXITCODE) { throw "ISCC failed" }
    Get-Item "target\installer\holo-setup-$version.exe"
} finally {
    Pop-Location
}
