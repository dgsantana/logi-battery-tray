//! Device battery records and low-battery alert decisions. Pure logic.

use std::collections::{BTreeMap, BTreeSet};

use crate::hidpp::{BatteryStatus, ChargingState, DeviceKind};

/// Alert when a discharging device drops to or below each of these.
pub const THRESHOLDS: [u8; 2] = [15, 5];

/// A device slot on one transport (receiver or direct BLE link).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceKey {
    /// Stable id of the transport the device is reached through.
    pub transport: String,
    /// HID++ device index on that transport.
    pub index: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub key: DeviceKey,
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
    entries: BTreeMap<DeviceKey, Entry>,
    /// Transports currently reachable; devices on others are hidden.
    present: BTreeSet<String>,
    /// Transports ever seen, so a later upsert does not re-show an absent one.
    known: BTreeSet<String>,
}

impl State {
    /// Insert or replace a device record; returns alerts its battery triggers.
    pub fn upsert(&mut self, device: Device) -> Vec<Alert> {
        if self.known.insert(device.key.transport.clone()) {
            self.present.insert(device.key.transport.clone());
        }
        let entry = self
            .entries
            .entry(device.key.clone())
            .and_modify(|e| e.device = device.clone())
            .or_insert_with(|| Entry { device, fired: [false; THRESHOLDS.len()] });
        entry.evaluate()
    }

    /// New battery reading for a known device (e.g. from an event); marks it online.
    pub fn set_battery(&mut self, key: &DeviceKey, battery: BatteryStatus) -> Vec<Alert> {
        let Some(entry) = self.entries.get_mut(key) else { return Vec::new() };
        entry.device.battery = Some(battery);
        entry.device.online = true;
        entry.evaluate()
    }

    pub fn set_online(&mut self, key: &DeviceKey, online: bool) {
        if let Some(entry) = self.entries.get_mut(key) {
            entry.device.online = online;
        }
    }

    /// Mark a transport reachable or gone; devices on a gone transport are hidden.
    pub fn set_present(&mut self, transport: &str, present: bool) {
        self.known.insert(transport.into());
        if present {
            self.present.insert(transport.into());
        } else {
            self.present.remove(transport);
        }
    }

    /// True when at least one transport is reachable.
    pub fn any_present(&self) -> bool {
        !self.present.is_empty()
    }

    fn visible(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values().filter(|e| self.present.contains(&e.device.key.transport))
    }

    pub fn snapshot(&self) -> Vec<Device> {
        self.visible().map(|e| e.device.clone()).collect()
    }

    /// Lowest battery among online devices with a known reading.
    pub fn lowest(&self) -> Option<BatteryStatus> {
        self.visible()
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
            } else if battery.charging == ChargingState::Discharging && !*fired {
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

    fn batt(percent: u8, charging: ChargingState) -> BatteryStatus {
        BatteryStatus { percent, charging }
    }

    fn key(transport: &str, index: u8) -> DeviceKey {
        DeviceKey { transport: transport.into(), index }
    }

    fn k(index: u8) -> DeviceKey {
        key("t", index)
    }

    fn dev(index: u8, percent: u8) -> Device {
        dev_on("t", index, percent)
    }

    fn dev_on(transport: &str, index: u8, percent: u8) -> Device {
        Device {
            key: key(transport, index),
            name: format!("dev{index}"),
            kind: DeviceKind::Mouse,
            battery: Some(batt(percent, ChargingState::Discharging)),
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
        let idx: Vec<u8> = s.snapshot().iter().map(|d| d.key.index).collect();
        assert_eq!(idx, [2, 4]);
    }

    #[test]
    fn lowest_ignores_offline_and_unknown() {
        let mut s = State::default();
        assert_eq!(s.lowest(), None);
        s.upsert(dev(2, 40));
        s.upsert(dev(4, 80));
        s.upsert(Device { battery: None, ..dev(5, 0) });
        assert_eq!(s.lowest(), Some(batt(40, ChargingState::Discharging)));
        s.set_online(&k(2), false);
        assert_eq!(s.lowest(), Some(batt(80, ChargingState::Discharging)));
    }

    #[test]
    fn offline_keeps_last_reading() {
        let mut s = State::default();
        s.upsert(dev(2, 40));
        s.set_online(&k(2), false);
        let d = &s.snapshot()[0];
        assert!(!d.online);
        assert_eq!(d.battery, Some(batt(40, ChargingState::Discharging)));
    }

    #[test]
    fn crossing_first_threshold_fires_once() {
        let mut s = State::default();
        assert!(s.upsert(dev(2, 20)).is_empty());
        assert_eq!(s.set_battery(&k(2), batt(15, ChargingState::Discharging)), alert(2, 15));
        assert!(s.set_battery(&k(2), batt(14, ChargingState::Discharging)).is_empty());
        assert!(s.set_battery(&k(2), batt(10, ChargingState::Discharging)).is_empty());
    }

    #[test]
    fn crossing_second_threshold_fires_again() {
        let mut s = State::default();
        s.upsert(dev(2, 12));
        assert_eq!(s.set_battery(&k(2), batt(5, ChargingState::Discharging)), alert(2, 5));
        assert!(s.set_battery(&k(2), batt(4, ChargingState::Discharging)).is_empty());
    }

    #[test]
    fn skipping_both_thresholds_fires_one_alert() {
        let mut s = State::default();
        assert_eq!(s.upsert(dev(2, 3)), alert(2, 3));
        assert!(s.set_battery(&k(2), batt(10, ChargingState::Discharging)).is_empty());
    }

    #[test]
    fn rising_above_threshold_rearms() {
        let mut s = State::default();
        s.upsert(dev(2, 10));
        s.set_battery(&k(2), batt(60, ChargingState::Discharging));
        assert_eq!(s.set_battery(&k(2), batt(15, ChargingState::Discharging)), alert(2, 15));
    }

    #[test]
    fn charging_suppresses_alerts() {
        let mut s = State::default();
        s.upsert(dev(2, 30));
        assert!(s.set_battery(&k(2), batt(10, ChargingState::Charging)).is_empty());
        assert!(s.set_battery(&k(2), batt(4, ChargingState::Full)).is_empty());
    }

    #[test]
    fn battery_reading_brings_device_online() {
        let mut s = State::default();
        s.upsert(dev(2, 40));
        s.set_online(&k(2), false);
        s.set_battery(&k(2), batt(39, ChargingState::Discharging));
        assert!(s.snapshot()[0].online);
    }

    #[test]
    fn set_battery_on_unknown_device_is_ignored() {
        let mut s = State::default();
        assert!(s.set_battery(&k(9), batt(1, ChargingState::Discharging)).is_empty());
        assert!(s.snapshot().is_empty());
    }

    #[test]
    fn upsert_preserves_alert_memory() {
        let mut s = State::default();
        s.upsert(dev(2, 10));
        // re-probe after reconnect, still low: no repeat
        assert!(s.upsert(dev(2, 9)).is_empty());
    }

    #[test]
    fn same_index_on_two_transports_stays_distinct() {
        let mut s = State::default();
        s.upsert(dev_on("a", 1, 50));
        s.upsert(dev_on("b", 1, 80));
        assert_eq!(s.snapshot().len(), 2);
        s.set_battery(&key("a", 1), batt(40, ChargingState::Discharging));
        s.set_online(&key("b", 1), false);
        let snap = s.snapshot();
        assert_eq!(snap[0].key, key("a", 1));
        assert_eq!(snap[0].battery, Some(batt(40, ChargingState::Discharging)));
        assert!(snap[0].online);
        assert_eq!(snap[1].battery, Some(batt(80, ChargingState::Discharging)));
        assert!(!snap[1].online);
    }

    #[test]
    fn absent_transport_hides_only_its_devices() {
        let mut s = State::default();
        s.upsert(dev_on("a", 1, 30));
        s.upsert(dev_on("b", 0xFF, 70));
        s.set_present("a", false);
        let keys: Vec<DeviceKey> = s.snapshot().into_iter().map(|d| d.key).collect();
        assert_eq!(keys, [key("b", 0xFF)]);
        assert_eq!(s.lowest(), Some(batt(70, ChargingState::Discharging)));
        assert!(s.any_present());
        s.set_present("a", true);
        assert_eq!(s.snapshot().len(), 2);
    }

    #[test]
    fn re_presented_transport_keeps_alert_memory() {
        let mut s = State::default();
        assert_eq!(s.upsert(dev_on("a", 1, 10)), alert(1, 10));
        s.set_present("a", false);
        s.set_present("a", true);
        assert!(s.upsert(dev_on("a", 1, 9)).is_empty());
    }

    #[test]
    fn any_present_tracks_transports() {
        let mut s = State::default();
        assert!(!s.any_present());
        s.upsert(dev_on("a", 1, 50));
        assert!(s.any_present());
        s.set_present("a", false);
        assert!(!s.any_present());
        s.set_present("b", true);
        assert!(s.any_present());
    }
}
