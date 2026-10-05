set windows-shell := ["powershell.exe", "-NoProfile", "-Command"]

# List recipes
default:
    @just --list

# Release build
build:
    cargo build --release

# Run the test suite (cargo-nextest)
test:
    cargo nextest run

# Clippy on all targets, warnings are errors
lint:
    cargo clippy --all-targets -- -D warnings

# Print battery levels once and exit
once:
    cargo run -- --once

# Clippy for the Windows target (needs: rustup target add x86_64-pc-windows-msvc)
[linux]
check-windows:
    cargo clippy --all-targets --target x86_64-pc-windows-msvc -- -D warnings

# Regenerate the Windows .ico from the SVG (maintenance; the .ico is committed)
[linux]
icon:
    magick -background none -density 384 packaging/logi-battery-tray.svg -define icon:auto-resize=256,48,32,16 packaging/logi-battery-tray.ico
    rsvg-convert -w 256 -h 256 packaging/logi-battery-tray.svg -o packaging/logi-battery-tray.png

# Install for this user: binary, launcher entry, icon, and start at login (`just install false` to skip it)
[linux]
[script("bash")]
install autostart="true":
    set -euo pipefail
    cargo install --path . --locked
    data="${XDG_DATA_HOME:-$HOME/.local/share}"
    config="${XDG_CONFIG_HOME:-$HOME/.config}"
    install -Dm644 packaging/logi-battery-tray.desktop "$data/applications/logi-battery-tray.desktop"
    install -Dm644 packaging/logi-battery-tray.svg "$data/icons/hicolor/scalable/apps/logi-battery-tray.svg"
    if [ "{{autostart}}" = "true" ]; then
        install -Dm644 packaging/logi-battery-tray.desktop "$config/autostart/logi-battery-tray.desktop"
    else
        rm -f "$config/autostart/logi-battery-tray.desktop"
    fi
    if command -v kbuildsycoca6 >/dev/null; then
        kbuildsycoca6 >/dev/null 2>&1
    elif command -v update-desktop-database >/dev/null; then
        update-desktop-database "$data/applications"
    fi

# Remove everything install put in place
[linux]
[script("bash")]
uninstall:
    set -euo pipefail
    data="${XDG_DATA_HOME:-$HOME/.local/share}"
    config="${XDG_CONFIG_HOME:-$HOME/.config}"
    cargo uninstall logi-battery-tray || true
    rm -f "$data/applications/logi-battery-tray.desktop" \
        "$data/icons/hicolor/scalable/apps/logi-battery-tray.svg" \
        "$config/autostart/logi-battery-tray.desktop"
    if command -v kbuildsycoca6 >/dev/null; then
        kbuildsycoca6 >/dev/null 2>&1
    elif command -v update-desktop-database >/dev/null; then
        update-desktop-database "$data/applications"
    fi

# Install for this user: binary, Start Menu shortcut, toast identity, and start at login (`just install false` to skip it)
[windows]
[script("powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File")]
[extension(".ps1")]
install autostart="true":
    $ErrorActionPreference = "Stop"
    # A running exe cannot be replaced; wait for it to exit.
    Get-Process logi-battery-tray -ErrorAction SilentlyContinue | Stop-Process -PassThru | Wait-Process
    cargo install --path . --locked
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE ".cargo" }
    $exe = Join-Path $cargoHome "bin\logi-battery-tray.exe"
    $dataDir = Join-Path $env:LOCALAPPDATA "logi-battery-tray"
    New-Item -ItemType Directory -Force $dataDir | Out-Null
    Copy-Item packaging\logi-battery-tray.png $dataDir -Force
    $lnk = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Logitech Battery Tray.lnk"
    $shortcut = (New-Object -ComObject WScript.Shell).CreateShortcut($lnk)
    $shortcut.TargetPath = $exe
    $shortcut.IconLocation = "$exe,0"
    $shortcut.Description = "Battery levels of Logitech devices"
    $shortcut.Save()
    $aumid = "HKCU:\Software\Classes\AppUserModelId\dgsantana.LogiBatteryTray"
    New-Item -Force $aumid | Out-Null
    Set-ItemProperty $aumid -Name DisplayName -Value "Logitech Battery Tray"
    Set-ItemProperty $aumid -Name IconUri -Value (Join-Path $dataDir "logi-battery-tray.png")
    $run = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
    if ("{{autostart}}" -eq "true") {
        Set-ItemProperty $run -Name LogiBatteryTray -Value "`"$exe`""
    } else {
        Remove-ItemProperty $run -Name LogiBatteryTray -ErrorAction SilentlyContinue
    }
    Start-Process $exe

# Remove everything install put in place
[windows]
[script("powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File")]
[extension(".ps1")]
uninstall:
    # Wait for exit: a running exe cannot be deleted.
    Get-Process logi-battery-tray -ErrorAction SilentlyContinue | Stop-Process -PassThru | Wait-Process
    cargo uninstall logi-battery-tray
    Remove-Item (Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Logitech Battery Tray.lnk") -ErrorAction SilentlyContinue
    Remove-ItemProperty "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name LogiBatteryTray -ErrorAction SilentlyContinue
    Remove-Item "HKCU:\Software\Classes\AppUserModelId\dgsantana.LogiBatteryTray" -Recurse -ErrorAction SilentlyContinue
    Remove-Item (Join-Path $env:LOCALAPPDATA "logi-battery-tray") -Recurse -ErrorAction SilentlyContinue
