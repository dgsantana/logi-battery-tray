//! Tray content shared by every platform: text, levels, snapshots.

use crate::hidpp::{BatteryStatus, ChargingState};
use crate::state::{Device, State};

pub const TITLE: &str = "Logitech batteries";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    Refresh,
    Quit,
}

/// What the UI shows: visible devices, the lowest battery, and whether any
/// transport is reachable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub devices: Vec<Device>,
    pub lowest: Option<BatteryStatus>,
    pub present: bool,
}

impl Snapshot {
    pub fn of(state: &State) -> Self {
        Self { devices: state.snapshot(), lowest: state.lowest(), present: state.any_present() }
    }
}

/// Battery percent rounded down to tens, as icons show it.
pub fn level(b: BatteryStatus) -> u8 {
    b.percent / 10 * 10
}

/// Breeze status icon for the lowest battery.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn icon_for(lowest: Option<BatteryStatus>) -> String {
    let Some(b) = lowest else { return "battery-missing".into() };
    let level = level(b);
    match b.charging {
        ChargingState::Charging | ChargingState::Full => format!("battery-{level:03}-charging"),
        ChargingState::Discharging | ChargingState::Error => format!("battery-{level:03}"),
    }
}

/// One human-readable line per device, used in tooltip and menu.
pub fn device_line(device: &Device) -> String {
    let name = &device.name;
    match (device.online, device.battery) {
        (true, Some(b)) => {
            let note = match b.charging {
                ChargingState::Discharging => "",
                ChargingState::Charging => " (charging)",
                ChargingState::Full => " (full)",
                ChargingState::Error => " (battery error)",
            };
            format!("{name} — {}%{note}", b.percent)
        }
        (true, None) => format!("{name} — battery unknown"),
        (false, Some(b)) => format!("{name} — offline (last {}%)", b.percent),
        (false, None) => format!("{name} — offline"),
    }
}

/// Tooltip body.
pub fn summary(devices: &[Device], present: bool) -> String {
    if !present {
        return "No Logitech devices found".into();
    }
    if devices.is_empty() {
        return "No devices found".into();
    }
    devices.iter().map(device_line).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hidpp::DeviceKind;
    use crate::state::DeviceKey;

    fn batt(percent: u8, charging: ChargingState) -> Option<BatteryStatus> {
        Some(BatteryStatus { percent, charging })
    }

    fn dev(name: &str, battery: Option<BatteryStatus>, online: bool) -> Device {
        Device { key: DeviceKey { transport: "t".into(), index: 2 }, name: name.into(), kind: DeviceKind::Mouse, battery, online }
    }

    #[test]
    fn icon_rounds_down_to_tens() {
        assert_eq!(icon_for(batt(95, ChargingState::Discharging)), "battery-090");
        assert_eq!(icon_for(batt(100, ChargingState::Discharging)), "battery-100");
        assert_eq!(icon_for(batt(5, ChargingState::Discharging)), "battery-000");
        assert_eq!(icon_for(batt(15, ChargingState::Error)), "battery-010");
    }

    #[test]
    fn icon_shows_charging_when_plugged() {
        assert_eq!(icon_for(batt(100, ChargingState::Charging)), "battery-100-charging");
        assert_eq!(icon_for(batt(100, ChargingState::Full)), "battery-100-charging");
        assert_eq!(icon_for(batt(42, ChargingState::Charging)), "battery-040-charging");
    }

    #[test]
    fn level_rounds_down_to_tens() {
        let level = |p| level(BatteryStatus { percent: p, charging: ChargingState::Discharging });
        assert_eq!(level(95), 90);
        assert_eq!(level(100), 100);
        assert_eq!(level(5), 0);
    }

    #[test]
    fn icon_missing_without_reading() {
        assert_eq!(icon_for(None), "battery-missing");
    }

    #[test]
    fn device_lines() {
        assert_eq!(device_line(&dev("MX Master 3S", batt(95, ChargingState::Discharging), true)), "MX Master 3S — 95%");
        assert_eq!(device_line(&dev("MX Keys S", batt(100, ChargingState::Charging), true)), "MX Keys S — 100% (charging)");
        assert_eq!(device_line(&dev("MX Keys S", batt(100, ChargingState::Full), true)), "MX Keys S — 100% (full)");
        assert_eq!(device_line(&dev("M", batt(30, ChargingState::Error), true)), "M — 30% (battery error)");
        assert_eq!(device_line(&dev("MX Keys S", batt(100, ChargingState::Discharging), false)), "MX Keys S — offline (last 100%)");
        assert_eq!(device_line(&dev("M", None, false)), "M — offline");
        assert_eq!(device_line(&dev("M", None, true)), "M — battery unknown");
    }

    #[test]
    fn summary_lists_devices_or_explains_absence() {
        let devices = [dev("A", batt(50, ChargingState::Discharging), true), dev("B", None, true)];
        assert_eq!(summary(&devices, true), "A — 50%\nB — battery unknown");
        assert_eq!(summary(&[], true), "No devices found");
        assert_eq!(summary(&devices, false), "No Logitech devices found");
    }

    #[test]
    fn snapshot_of_state_with_one_transport_gone() {
        let mut state = crate::state::State::default();
        let mut a = dev("A", batt(20, ChargingState::Discharging), true);
        a.key.transport = "a".into();
        let mut b = dev("B", batt(60, ChargingState::Discharging), true);
        b.key.transport = "b".into();
        state.upsert(a);
        state.upsert(b);
        state.set_present("a", false);
        let snap = Snapshot::of(&state);
        assert!(snap.present);
        assert_eq!(snap.devices.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["B"]);
        assert_eq!(snap.lowest, batt(60, ChargingState::Discharging));
        state.set_present("b", false);
        assert!(!Snapshot::of(&state).present);
    }
}
