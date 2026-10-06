# Install Logitech Battery Tray for the current user: Start Menu shortcut,
# toast identity, and start at login.
#
# From a release zip: run with no -Exe; the exe beside this script is copied to
# %LOCALAPPDATA%\Programs\logi-battery-tray. `just install` passes -Exe with the
# cargo-installed binary instead.
param(
    [string]$Exe,
    [switch]$NoAutostart
)
$ErrorActionPreference = "Stop"

# A running exe cannot be replaced; wait for it to exit.
Get-Process logi-battery-tray -ErrorAction SilentlyContinue | Stop-Process -PassThru | Wait-Process

if (-not $Exe) {
    $programDir = Join-Path $env:LOCALAPPDATA "Programs\logi-battery-tray"
    New-Item -ItemType Directory -Force $programDir | Out-Null
    Copy-Item (Join-Path $PSScriptRoot "logi-battery-tray.exe") $programDir -Force
    $Exe = Join-Path $programDir "logi-battery-tray.exe"
}

$dataDir = Join-Path $env:LOCALAPPDATA "logi-battery-tray"
New-Item -ItemType Directory -Force $dataDir | Out-Null
Copy-Item (Join-Path $PSScriptRoot "logi-battery-tray.png") $dataDir -Force

$lnk = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Logitech Battery Tray.lnk"
$shortcut = (New-Object -ComObject WScript.Shell).CreateShortcut($lnk)
$shortcut.TargetPath = $Exe
$shortcut.IconLocation = "$Exe,0"
$shortcut.Description = "Battery levels of Logitech devices"
$shortcut.Save()

# Toasts from an unregistered AppUserModelID are dropped by Windows.
$aumid = "HKCU:\Software\Classes\AppUserModelId\dgsantana.LogiBatteryTray"
New-Item -Force $aumid | Out-Null
Set-ItemProperty $aumid -Name DisplayName -Value "Logitech Battery Tray"
Set-ItemProperty $aumid -Name IconUri -Value (Join-Path $dataDir "logi-battery-tray.png")

$run = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
if ($NoAutostart) {
    Remove-ItemProperty $run -Name LogiBatteryTray -ErrorAction SilentlyContinue
} else {
    Set-ItemProperty $run -Name LogiBatteryTray -Value "`"$Exe`""
}

Start-Process $Exe
