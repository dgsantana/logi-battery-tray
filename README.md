# logi-battery-tray

Tray icon for KDE Plasma 6 showing battery levels of Logitech devices paired to a
Logi Bolt receiver (`046d:c548`). The kernel does not bind `hid-logitech-dj` to the
Bolt receiver, so UPower and Plasma's battery widget never see these devices.

- Icon follows the lowest online battery; tooltip and menu list every device.
- Notification when a discharging device drops to 15% and again at 5%.
- Live updates from device battery events, plus a re-poll every 5 minutes.

Talks HID++ 2.0 directly over `/dev/hidraw` (no Solaar needed; don't run both at once).
Access comes from the logind `uaccess` ACL on the receiver's hidraw nodes.

## Install

    cargo install --path .
    cp packaging/logi-battery-tray.desktop ~/.local/share/applications/  # launcher
    cp packaging/logi-battery-tray.desktop ~/.config/autostart/          # start at login

## Use

    logi-battery-tray            # tray icon
    logi-battery-tray --once     # print levels and exit
    RUST_LOG=debug logi-battery-tray

## Develop

Recipes use [just](https://github.com/casey/just):

    just test     # cargo test
    just lint     # clippy, warnings are errors
    just once     # run --once from source
    just build    # release build

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
