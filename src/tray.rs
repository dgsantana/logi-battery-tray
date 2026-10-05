//! StatusNotifierItem tray rendering.

use std::sync::mpsc::Sender;

use ksni::menu::{MenuItem, StandardItem};
use ksni::{Category, ToolTip};

use crate::hidpp::{BatteryStatus, Charging, DeviceKind};
use crate::state::Device;

pub const TITLE: &str = "Logitech batteries";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    Refresh,
    Quit,
}

pub struct BatteryTray {
    pub devices: Vec<Device>,
    pub lowest: Option<BatteryStatus>,
    pub receiver_present: bool,
    pub tx: Sender<Cmd>,
}

/// Breeze status icon for the lowest battery.
pub fn icon_for(lowest: Option<BatteryStatus>) -> String {
    let Some(b) = lowest else { return "battery-missing".into() };
    let level = b.percent / 10 * 10;
    match b.charging {
        Charging::Charging | Charging::Full => format!("battery-{level:03}-charging"),
        Charging::Discharging | Charging::Error => format!("battery-{level:03}"),
    }
}

/// One human-readable line per device, used in tooltip and menu.
pub fn device_line(device: &Device) -> String {
    let name = &device.name;
    match (device.online, device.battery) {
        (true, Some(b)) => {
            let note = match b.charging {
                Charging::Discharging => "",
                Charging::Charging => " (charging)",
                Charging::Full => " (full)",
                Charging::Error => " (battery error)",
            };
            format!("{name} — {}%{note}", b.percent)
        }
        (true, None) => format!("{name} — battery unknown"),
        (false, Some(b)) => format!("{name} — offline (last {}%)", b.percent),
        (false, None) => format!("{name} — offline"),
    }
}

/// Tooltip body.
pub fn summary(devices: &[Device], receiver_present: bool) -> String {
    if !receiver_present {
        return "Receiver not found".into();
    }
    if devices.is_empty() {
        return "No devices found".into();
    }
    devices.iter().map(device_line).collect::<Vec<_>>().join("\n")
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
        icon_for(self.lowest.filter(|_| self.receiver_present))
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            icon_name: self.icon_name(),
            title: TITLE.into(),
            description: summary(&self.devices, self.receiver_present),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = if self.devices.is_empty() {
            vec![StandardItem {
                label: summary(&self.devices, self.receiver_present),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn batt(percent: u8, charging: Charging) -> Option<BatteryStatus> {
        Some(BatteryStatus { percent, charging })
    }

    fn dev(name: &str, battery: Option<BatteryStatus>, online: bool) -> Device {
        Device { index: 2, name: name.into(), kind: DeviceKind::Mouse, battery, online }
    }

    #[test]
    fn icon_rounds_down_to_tens() {
        assert_eq!(icon_for(batt(95, Charging::Discharging)), "battery-090");
        assert_eq!(icon_for(batt(100, Charging::Discharging)), "battery-100");
        assert_eq!(icon_for(batt(5, Charging::Discharging)), "battery-000");
        assert_eq!(icon_for(batt(15, Charging::Error)), "battery-010");
    }

    #[test]
    fn icon_shows_charging_when_plugged() {
        assert_eq!(icon_for(batt(100, Charging::Charging)), "battery-100-charging");
        assert_eq!(icon_for(batt(100, Charging::Full)), "battery-100-charging");
        assert_eq!(icon_for(batt(42, Charging::Charging)), "battery-040-charging");
    }

    #[test]
    fn icon_missing_without_reading() {
        assert_eq!(icon_for(None), "battery-missing");
    }

    #[test]
    fn device_lines() {
        assert_eq!(device_line(&dev("MX Master 3S", batt(95, Charging::Discharging), true)), "MX Master 3S — 95%");
        assert_eq!(device_line(&dev("MX Keys S", batt(100, Charging::Charging), true)), "MX Keys S — 100% (charging)");
        assert_eq!(device_line(&dev("MX Keys S", batt(100, Charging::Full), true)), "MX Keys S — 100% (full)");
        assert_eq!(device_line(&dev("M", batt(30, Charging::Error), true)), "M — 30% (battery error)");
        assert_eq!(device_line(&dev("MX Keys S", batt(100, Charging::Discharging), false)), "MX Keys S — offline (last 100%)");
        assert_eq!(device_line(&dev("M", None, false)), "M — offline");
        assert_eq!(device_line(&dev("M", None, true)), "M — battery unknown");
    }

    #[test]
    fn summary_lists_devices_or_explains_absence() {
        let devices = [dev("A", batt(50, Charging::Discharging), true), dev("B", None, true)];
        assert_eq!(summary(&devices, true), "A — 50%\nB — battery unknown");
        assert_eq!(summary(&[], true), "No devices found");
        assert_eq!(summary(&devices, false), "Receiver not found");
    }
}
