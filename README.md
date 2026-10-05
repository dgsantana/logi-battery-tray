# logi-battery-tray

Tray icon showing battery levels of Logitech devices, on KDE Plasma 6 and
Windows 11. Works with devices paired to a Logi Bolt receiver (`046d:c548`) and
with devices connected directly over Bluetooth LE. On Linux the kernel does not
bind `hid-logitech-dj` to the Bolt receiver, so UPower and Plasma's battery
widget never see these devices.

- Icon follows the lowest online battery; tooltip and menu list every device.
- Notification when a discharging device drops to 15% and again at 5%.
- Live updates from device battery events, plus a re-poll every 5 minutes.
- Only one instance runs; starting it again does nothing.

Talks HID++ 2.0 directly over HID (no Solaar or Logi Options+ needed; don't run
them at the same time). On Linux, access comes from the logind `uaccess` ACL on
the receiver's hidraw nodes; on Windows no admin rights are needed.

## Install

Needs Rust and [just](https://github.com/casey/just)
(`pacman -S just`, `winget install Casey.Just`, or `mise use -g just`).

    just install                   # binary, launcher/Start Menu entry, icon, start at login
    just install autostart=false   # same, without start at login
    just uninstall                 # remove all of it

Linux: installs to `~/.cargo/bin`, the desktop entry and icon under
`~/.local/share`, and autostart under `~/.config/autostart`.
Windows: installs to `%USERPROFILE%\.cargo\bin`, a Start Menu shortcut, a
`HKCU\...\Run` entry for start at login, and registers the app so
notifications show its name and icon.

## Use

    logi-battery-tray            # tray icon
    logi-battery-tray --once     # print levels and exit
    RUST_LOG=debug logi-battery-tray

## Develop

Recipes use [just](https://github.com/casey/just); tests run with
[cargo-nextest](https://nexte.st) (`cargo install cargo-nextest --locked`):

    just test     # cargo nextest run
    just lint     # clippy, warnings are errors
    just once     # run --once from source
    just build    # release build
    just check-windows   # clippy for the Windows target, from Linux

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
