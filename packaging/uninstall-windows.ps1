# Remove everything install-windows.ps1 put in place. `just uninstall` also
# removes the cargo-installed binary.

# Wait for exit: a running exe cannot be deleted.
Get-Process logi-battery-tray -ErrorAction SilentlyContinue | Stop-Process -PassThru | Wait-Process
Remove-Item (Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Logitech Battery Tray.lnk") -ErrorAction SilentlyContinue
Remove-ItemProperty "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run" -Name LogiBatteryTray -ErrorAction SilentlyContinue
Remove-Item "HKCU:\Software\Classes\AppUserModelId\dgsantana.LogiBatteryTray" -Recurse -ErrorAction SilentlyContinue
Remove-Item (Join-Path $env:LOCALAPPDATA "logi-battery-tray") -Recurse -ErrorAction SilentlyContinue
Remove-Item (Join-Path $env:LOCALAPPDATA "Programs\logi-battery-tray") -Recurse -ErrorAction SilentlyContinue
