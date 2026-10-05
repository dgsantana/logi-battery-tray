//! KDE/freedesktop: StatusNotifierItem tray (ksni) and desktop notifications.

use std::process::ExitCode;
use std::sync::mpsc::Sender;

use ksni::blocking::TrayMethods;
use ksni::menu::{CheckmarkItem, MenuItem, StandardItem};
use ksni::{Category, ToolTip};
use log::{error, info, warn};

use super::Publisher;
use crate::autostart;
use crate::hidpp::{BatteryStatus, DeviceKind};
use crate::state::{Alert, Device};
use crate::tray::{self, device_line, icon_for, summary, Cmd, TITLE};

pub struct BatteryTray {
    pub devices: Vec<Device>,
    pub lowest: Option<BatteryStatus>,
    pub present: bool,
    pub tx: Sender<Cmd>,
}

impl ksni::Tray for BatteryTray {
    fn id(&self) -> String {
        env!("CARGO_PKG_NAME").into()
    }

    fn title(&self) -> String {
        TITLE.into()
    }

    fn category(&self) -> Category {
        Category::Hardware
    }

    fn icon_name(&self) -> String {
        icon_for(self.lowest.filter(|_| self.present))
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            icon_name: self.icon_name(),
            title: TITLE.into(),
            description: summary(&self.devices, self.present),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = if self.devices.is_empty() || !self.present {
            vec![StandardItem {
                label: summary(&self.devices, self.present),
                enabled: false,
                ..Default::default()
            }
            .into()]
        } else {
            self.devices
                .iter()
                .map(|d| {
                    StandardItem {
                        label: device_line(d),
                        enabled: false,
                        icon_name: match d.kind {
                            DeviceKind::Keyboard => "input-keyboard",
                            DeviceKind::Mouse => "input-mouse",
                            DeviceKind::Other => "input-gaming",
                        }
                        .into(),
                        ..Default::default()
                    }
                    .into()
                })
                .collect()
        };
        items.push(MenuItem::Separator);
        items.push(
            CheckmarkItem {
                label: "Start at login".into(),
                checked: autostart::is_enabled(),
                activate: Box::new(|_: &mut Self| {
                    let on = !autostart::is_enabled();
                    if let Err(e) = autostart::set_enabled(on) {
                        warn!("cannot change start at login: {e}");
                    }
                }),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Refresh".into(),
                icon_name: "view-refresh".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(Cmd::Refresh);
                }),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(Cmd::Quit);
                }),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}

/// Output already goes to the terminal on Linux.
pub fn attach_console() {}

/// Logs go to stderr (the session journal for autostarted apps).
pub fn log_file() -> Option<std::fs::File> {
    None
}

/// Start the tray, then run the worker on this thread until it returns.
pub fn run(cmd_tx: Sender<Cmd>, worker: Box<dyn FnOnce(Publisher) + Send>) -> ExitCode {
    let tray = BatteryTray { devices: Vec::new(), lowest: None, present: false, tx: cmd_tx };
    let handle = match tray.spawn() {
        Ok(handle) => handle,
        Err(e) => {
            error!("cannot start tray: {e}");
            return ExitCode::FAILURE;
        }
    };
    let publisher = handle.clone();
    worker(Box::new(move |snap| {
        publisher.update(|t| {
            t.devices = snap.devices;
            t.lowest = snap.lowest;
            t.present = snap.present;
        });
    }));
    handle.shutdown().wait();
    ExitCode::SUCCESS
}

pub fn notify(alert: &Alert) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hidpp::ChargingState;
    use crate::state::DeviceKey;

    fn batt(percent: u8, charging: ChargingState) -> Option<BatteryStatus> {
        Some(BatteryStatus { percent, charging })
    }

    fn dev(name: &str, battery: Option<BatteryStatus>, online: bool) -> Device {
        Device { key: DeviceKey { transport: "t".into(), index: 2 }, name: name.into(), kind: DeviceKind::Mouse, battery, online }
    }

    fn menu_labels(tray: &BatteryTray) -> Vec<String> {
        use ksni::Tray;
        tray.menu()
            .into_iter()
            .filter_map(|i| match i {
                MenuItem::Standard(s) => Some(s.label),
                MenuItem::Checkmark(c) => Some(c.label),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn menu_hides_stale_devices_without_receiver() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut tray = BatteryTray {
            devices: vec![dev("A", batt(50, ChargingState::Discharging), true)],
            lowest: batt(50, ChargingState::Discharging),
            present: true,
            tx,
        };
        assert_eq!(menu_labels(&tray), ["A — 50%", "Start at login", "Refresh", "Quit"]);
        tray.present = false;
        assert_eq!(menu_labels(&tray), ["No Logitech devices found", "Start at login", "Refresh", "Quit"]);
    }
}
