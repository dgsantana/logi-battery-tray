//! Device battery records and low-battery alert decisions. Pure logic.

use std::collections::BTreeMap;

use crate::hidpp::{BatteryStatus, Charging, DeviceKind};

/// Alert when a discharging device drops to or below each of these.
pub const THRESHOLDS: [u8; 2] = [15, 5];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub index: u8,
    pub name: String,
    pub kind: DeviceKind,
    /// Last known reading; kept while offline.
    pub battery: Option<BatteryStatus>,
    pub online: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    pub name: String,
    pub percent: u8,
}

#[derive(Debug)]
struct Entry {
    device: Device,
    fired: [bool; THRESHOLDS.len()],
}

#[derive(Debug, Default)]
pub struct State {
    entries: BTreeMap<u8, Entry>,
}

impl State {
    /// Insert or replace a device record; returns alerts its battery triggers.
    pub fn upsert(&mut self, device: Device) -> Vec<Alert> {
        let entry = self
            .entries
            .entry(device.index)
            .and_modify(|e| e.device = device.clone())
            .or_insert_with(|| Entry { device, fired: [false; THRESHOLDS.len()] });
        entry.evaluate()
    }

    /// New battery reading for a known device (e.g. from an event).
    pub fn set_battery(&mut self, index: u8, battery: BatteryStatus) -> Vec<Alert> {
        let Some(entry) = self.entries.get_mut(&index) else { return Vec::new() };
        entry.device.battery = Some(battery);
        entry.evaluate()
    }

    pub fn set_online(&mut self, index: u8, online: bool) {
        if let Some(entry) = self.entries.get_mut(&index) {
            entry.device.online = online;
        }
    }

    pub fn snapshot(&self) -> Vec<Device> {
        self.entries.values().map(|e| e.device.clone()).collect()
    }

    /// Lowest battery among online devices with a known reading.
    pub fn lowest(&self) -> Option<BatteryStatus> {
        self.entries
            .values()
            .filter(|e| e.device.online)
            .filter_map(|e| e.device.battery)
            .min_by_key(|b| b.percent)
    }
}

impl Entry {
    /// Re-arm thresholds the battery is back above, then fire at most one
    /// alert for thresholds newly crossed while discharging.
    fn evaluate(&mut self) -> Vec<Alert> {
        let Some(battery) = self.device.battery else { return Vec::new() };
        let mut crossed = false;
        for (fired, &threshold) in self.fired.iter_mut().zip(&THRESHOLDS) {
            if battery.percent > threshold {
                *fired = false;
            } else if battery.charging == Charging::Discharging && !*fired {
                *fired = true;
                crossed = true;
            }
        }
        if !crossed {
            return Vec::new();
        }
        vec![Alert { name: self.device.name.clone(), percent: battery.percent }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batt(percent: u8, charging: Charging) -> BatteryStatus {
        BatteryStatus { percent, charging }
    }

    fn dev(index: u8, percent: u8) -> Device {
        Device {
            index,
            name: format!("dev{index}"),
            kind: DeviceKind::Mouse,
            battery: Some(batt(percent, Charging::Discharging)),
            online: true,
        }
    }

    fn alert(index: u8, percent: u8) -> Vec<Alert> {
        vec![Alert { name: format!("dev{index}"), percent }]
    }

    #[test]
    fn snapshot_is_ordered_by_index() {
        let mut s = State::default();
        s.upsert(dev(4, 100));
        s.upsert(dev(2, 95));
        let idx: Vec<u8> = s.snapshot().iter().map(|d| d.index).collect();
        assert_eq!(idx, [2, 4]);
    }

    #[test]
    fn lowest_ignores_offline_and_unknown() {
        let mut s = State::default();
        assert_eq!(s.lowest(), None);
        s.upsert(dev(2, 40));
        s.upsert(dev(4, 80));
        s.upsert(Device { battery: None, ..dev(5, 0) });
        assert_eq!(s.lowest(), Some(batt(40, Charging::Discharging)));
        s.set_online(2, false);
        assert_eq!(s.lowest(), Some(batt(80, Charging::Discharging)));
    }

    #[test]
    fn offline_keeps_last_reading() {
        let mut s = State::default();
        s.upsert(dev(2, 40));
        s.set_online(2, false);
        let d = &s.snapshot()[0];
        assert!(!d.online);
        assert_eq!(d.battery, Some(batt(40, Charging::Discharging)));
    }

    #[test]
    fn crossing_first_threshold_fires_once() {
        let mut s = State::default();
        assert!(s.upsert(dev(2, 20)).is_empty());
        assert_eq!(s.set_battery(2, batt(15, Charging::Discharging)), alert(2, 15));
        assert!(s.set_battery(2, batt(14, Charging::Discharging)).is_empty());
        assert!(s.set_battery(2, batt(10, Charging::Discharging)).is_empty());
    }

    #[test]
    fn crossing_second_threshold_fires_again() {
        let mut s = State::default();
        s.upsert(dev(2, 12));
        assert_eq!(s.set_battery(2, batt(5, Charging::Discharging)), alert(2, 5));
        assert!(s.set_battery(2, batt(4, Charging::Discharging)).is_empty());
    }

    #[test]
    fn skipping_both_thresholds_fires_one_alert() {
        let mut s = State::default();
        assert_eq!(s.upsert(dev(2, 3)), alert(2, 3));
        assert!(s.set_battery(2, batt(10, Charging::Discharging)).is_empty());
    }

    #[test]
    fn rising_above_threshold_rearms() {
        let mut s = State::default();
        s.upsert(dev(2, 10));
        s.set_battery(2, batt(60, Charging::Discharging));
        assert_eq!(s.set_battery(2, batt(15, Charging::Discharging)), alert(2, 15));
    }

    #[test]
    fn charging_suppresses_alerts() {
        let mut s = State::default();
        s.upsert(dev(2, 30));
        assert!(s.set_battery(2, batt(10, Charging::Charging)).is_empty());
        assert!(s.set_battery(2, batt(4, Charging::Full)).is_empty());
    }

    #[test]
    fn set_battery_on_unknown_device_is_ignored() {
        let mut s = State::default();
        assert!(s.set_battery(9, batt(1, Charging::Discharging)).is_empty());
        assert!(s.snapshot().is_empty());
    }

    #[test]
    fn upsert_preserves_alert_memory() {
        let mut s = State::default();
        s.upsert(dev(2, 10));
        // re-probe after reconnect, still low: no repeat
        assert!(s.upsert(dev(2, 9)).is_empty());
    }
}
