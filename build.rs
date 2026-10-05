//! Embeds the app icon and version info into the Windows executable.

fn main() {
    println!("cargo:rerun-if-changed=packaging/logi-battery-tray.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resources();
    }
}

/// Resource compilation needs the Windows SDK tools, so it only runs when the
/// build host is Windows too; cross-target checks from Linux skip it.
#[cfg(windows)]
fn embed_resources() {
    let mut res = winresource::WindowsResource::new();
    res.set_icon("packaging/logi-battery-tray.ico")
        .set("ProductName", "Logitech Battery Tray")
        .set("FileDescription", "Battery levels of Logitech devices");
    res.compile().expect("cannot embed Windows resources");
}

#[cfg(not(windows))]
fn embed_resources() {}
