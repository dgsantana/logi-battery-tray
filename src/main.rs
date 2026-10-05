mod hidpp;
mod state;
mod transport;
mod worker;
mod tray;

use std::process::ExitCode;
use std::sync::mpsc;

use ksni::blocking::TrayMethods;
use log::{error, info, warn};

use crate::state::Alert;
use crate::tray::BatteryTray;

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    if std::env::args().any(|a| a == "--once") {
        return worker::run_once();
    }

    let (tx, rx) = mpsc::channel();
    let tray = BatteryTray { devices: Vec::new(), lowest: None, present: false, tx };
    let handle = match tray.spawn() {
        Ok(handle) => handle,
        Err(e) => {
            error!("cannot start tray: {e}");
            return ExitCode::FAILURE;
        }
    };
    worker::run(
        &rx,
        |snap| {
            handle.update(|t| {
                t.devices = snap.devices;
                t.lowest = snap.lowest;
                t.present = snap.present;
            });
        },
        notify,
    );
    handle.shutdown().wait();
    ExitCode::SUCCESS
}

fn notify(alert: &Alert) {
    info!("low battery: {} {}%", alert.name, alert.percent);
    let shown = notify_rust::Notification::new()
        .appname(tray::TITLE)
        .summary(&format!("{} battery low", alert.name))
        .body(&format!("{}% remaining", alert.percent))
        .icon("battery-caution")
        .show();
    if let Err(e) = shown {
        warn!("cannot show notification: {e}");
    }
}
