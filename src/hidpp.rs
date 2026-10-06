//! Minimal HID++ 2.0 message encoding/decoding. No I/O.

pub const REPORT_SHORT: u8 = 0x10;
pub const REPORT_LONG: u8 = 0x11;
pub const SHORT_LEN: usize = 7;
pub const LONG_LEN: usize = 20;

/// Software id stamped on our requests; device-initiated events carry 0.
pub const SWID: u8 = 0x0A;

pub const FEATURE_DEVICE_NAME: u16 = 0x0005;
pub const FEATURE_BATTERY_STATUS: u16 = 0x1000;
pub const FEATURE_UNIFIED_BATTERY: u16 = 0x1004;

/// HID++ 1.0 sub-id for the receiver's device connection notification.
const SUB_ID_CONNECTION: u8 = 0x41;
const SUB_ID_ERROR_10: u8 = 0x8F;
const FEAT_IDX_ERROR_20: u8 = 0xFF;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// Answer to one of our requests (swid != 0).
    Reply { dev: u8, feat_idx: u8, func: u8, swid: u8, data: Vec<u8> },
    /// Device-initiated notification (swid == 0).
    Event { dev: u8, feat_idx: u8, func: u8, data: Vec<u8> },
    /// HID++ 2.0 error for request (feat_idx, func, swid).
    Error20 { dev: u8, feat_idx: u8, func: u8, swid: u8, code: u8 },
    /// HID++ 1.0 error, e.g. device not paired or not reachable.
    Error10 { dev: u8, feat_idx: u8, func: u8, swid: u8, code: u8 },
    /// Receiver reports a device link coming up or going down.
    Connection { dev: u8, linked: bool },
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChargingState {
    Discharging,
    Charging,
    Full,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatteryStatus {
    pub percent: u8,
    pub charging: ChargingState,
}

/// Battery feature a device exposes, with its index in the device's feature table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatteryFeature {
    /// UNIFIED_BATTERY (0x1004): newer devices.
    Unified(u8),
    /// BATTERY_STATUS (0x1000): older devices, e.g. MX Vertical.
    Status(u8),
}

impl BatteryFeature {
    pub fn index(self) -> u8 {
        match self {
            Self::Unified(idx) | Self::Status(idx) => idx,
        }
    }

    /// Function that reads the current level.
    pub fn read_func(self) -> u8 {
        match self {
            Self::Unified(_) => 1,
            Self::Status(_) => 0,
        }
    }

    /// Decode a read reply or a battery event (function 0); each feature
    /// uses one payload layout for both.
    pub fn parse(self, data: &[u8]) -> Option<BatteryStatus> {
        match self {
            Self::Unified(_) => parse_battery(data),
            Self::Status(_) => parse_battery_status(data),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Keyboard,
    Mouse,
    Other,
}

/// Build a short request. At most 3 parameter bytes.
pub fn request(dev: u8, feat_idx: u8, func: u8, params: &[u8]) -> [u8; SHORT_LEN] {
    assert!(params.len() <= SHORT_LEN - 4, "too many params for short report");
    let mut buf = [REPORT_SHORT, dev, feat_idx, (func << 4) | SWID, 0, 0, 0];
    buf[4..4 + params.len()].copy_from_slice(params);
    buf
}

/// Build a long request, for links that only accept long reports (BLE).
pub fn request_long(dev: u8, feat_idx: u8, func: u8, params: &[u8]) -> [u8; LONG_LEN] {
    assert!(params.len() <= LONG_LEN - 4, "too many params for long report");
    let mut buf = [0; LONG_LEN];
    buf[..4].copy_from_slice(&[REPORT_LONG, dev, feat_idx, (func << 4) | SWID]);
    buf[4..4 + params.len()].copy_from_slice(params);
    buf
}

pub fn parse(buf: &[u8]) -> Option<Message> {
    let len = match *buf.first()? {
        REPORT_SHORT => SHORT_LEN,
        REPORT_LONG => LONG_LEN,
        _ => return None,
    };
    if buf.len() < len {
        return None;
    }
    let (report, dev, idx, b3) = (buf[0], buf[1], buf[2], buf[3]);
    let (func, swid) = (b3 >> 4, b3 & 0x0F);
    let msg = match (report, idx) {
        (REPORT_SHORT, SUB_ID_ERROR_10) => {
            let (func, swid) = (buf[4] >> 4, buf[4] & 0x0F);
            Message::Error10 { dev, feat_idx: b3, func, swid, code: buf[5] }
        }
        (_, FEAT_IDX_ERROR_20) => {
            let (func, swid) = (buf[4] >> 4, buf[4] & 0x0F);
            Message::Error20 { dev, feat_idx: b3, func, swid, code: buf[5] }
        }
        (REPORT_SHORT, SUB_ID_CONNECTION) => Message::Connection { dev, linked: buf[4] & 0x40 == 0 },
        // Remaining HID++ 1.0 notifications and register access.
        (_, 0x40..) => Message::Other,
        _ if swid == 0 => Message::Event { dev, feat_idx: idx, func, data: buf[4..len].to_vec() },
        _ => Message::Reply { dev, feat_idx: idx, func, swid, data: buf[4..len].to_vec() },
    };
    Some(msg)
}

/// Decode UNIFIED_BATTERY get_status reply / battery event payload.
pub fn parse_battery(data: &[u8]) -> Option<BatteryStatus> {
    let (&percent, &code) = (data.first()?, data.get(2)?);
    if percent > 100 {
        return None;
    }
    let charging = match code {
        0 => ChargingState::Discharging,
        1 | 2 => ChargingState::Charging,
        3 => ChargingState::Full,
        _ => ChargingState::Error,
    };
    Some(BatteryStatus { percent, charging })
}

/// Decode BATTERY_STATUS get_level reply / battery event payload:
/// level %, next level %, status.
pub fn parse_battery_status(data: &[u8]) -> Option<BatteryStatus> {
    let (&percent, &code) = (data.first()?, data.get(2)?);
    if percent > 100 {
        return None;
    }
    let charging = match code {
        0 => ChargingState::Discharging,
        // recharging, almost full, slow recharge
        1 | 2 | 4 => ChargingState::Charging,
        3 => ChargingState::Full,
        _ => ChargingState::Error,
    };
    Some(BatteryStatus { percent, charging })
}

/// Decode ROOT GetFeature reply; `None` when the feature is absent.
pub fn parse_feature_index(data: &[u8]) -> Option<u8> {
    data.first().copied().filter(|&i| i != 0)
}

impl DeviceKind {
    /// From DEVICE_NAME getDeviceType.
    pub fn from_type(t: u8) -> Self {
        match t {
            0 => Self::Keyboard,
            // mouse, trackpad, trackball
            3..=5 => Self::Mouse,
            _ => Self::Other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace().map(|b| u8::from_str_radix(b, 16).unwrap()).collect()
    }

    #[test]
    fn request_encodes_short_report_with_swid() {
        assert_eq!(request(2, 0, 0, &[0x10, 0x04]), [0x10, 0x02, 0x00, 0x0A, 0x10, 0x04, 0x00]);
        assert_eq!(request(4, 8, 1, &[]), [0x10, 0x04, 0x08, 0x1A, 0, 0, 0]);
    }

    #[test]
    fn request_long_encodes_long_report_with_swid() {
        let req = request_long(0xFF, 0x02, 1, &[0x05]);
        assert_eq!(req.len(), LONG_LEN);
        assert_eq!(req[..5], [0x11, 0xFF, 0x02, 0x1A, 0x05]);
        assert!(req[5..].iter().all(|&b| b == 0));
    }

    #[test]
    fn parses_get_feature_reply() {
        // probe: 0x1004 on MX Master 3S lives at index 8
        let m = parse(&hex("11 02 00 0a 08 00 03 00 00 00 00 00 00 00 00 00 00 00 00 00")).unwrap();
        let Message::Reply { dev, feat_idx, func, swid, data } = m else { panic!("{m:?}") };
        assert_eq!((dev, feat_idx, func, swid), (2, 0, 0, SWID));
        assert_eq!(parse_feature_index(&data), Some(8));
    }

    #[test]
    fn feature_index_zero_means_absent() {
        assert_eq!(parse_feature_index(&[0, 0, 0]), None);
        assert_eq!(parse_feature_index(&[]), None);
    }

    #[test]
    fn parses_mouse_battery_discharging() {
        let m = parse(&hex("11 02 08 1a 5f 08 00 00 00 00 00 00 00 00 00 00 00 00 00 00")).unwrap();
        let Message::Reply { func: 1, data, .. } = m else { panic!("{m:?}") };
        assert_eq!(
            parse_battery(&data),
            Some(BatteryStatus { percent: 95, charging: ChargingState::Discharging })
        );
    }

    #[test]
    fn parses_keyboard_battery_charging() {
        let m = parse(&hex("11 04 08 1a 64 08 01 01 00 00 00 00 00 00 00 00 00 00 00 00")).unwrap();
        let Message::Reply { data, .. } = m else { panic!("{m:?}") };
        assert_eq!(
            parse_battery(&data),
            Some(BatteryStatus { percent: 100, charging: ChargingState::Charging })
        );
    }

    #[test]
    fn battery_charging_codes() {
        assert_eq!(parse_battery(&[50, 4, 2, 1]).unwrap().charging, ChargingState::Charging);
        assert_eq!(parse_battery(&[100, 8, 3, 1]).unwrap().charging, ChargingState::Full);
        assert_eq!(parse_battery(&[50, 4, 4, 0]).unwrap().charging, ChargingState::Error);
    }

    #[test]
    fn battery_rejects_short_or_bogus_payload() {
        assert_eq!(parse_battery(&[50, 4]), None);
        assert_eq!(parse_battery(&[101, 8, 0]), None);
    }

    #[test]
    fn parses_battery_status_level() {
        // probe: MX Vertical over BLE, BATTERY_STATUS get_level at index 8
        let m = parse(&hex("11 ff 08 0a 32 14 00 00 00 00 00 00 00 00 00 00 00 00 00 00")).unwrap();
        let Message::Reply { func: 0, data, .. } = m else { panic!("{m:?}") };
        assert_eq!(
            BatteryFeature::Status(8).parse(&data),
            Some(BatteryStatus { percent: 50, charging: ChargingState::Discharging })
        );
    }

    #[test]
    fn battery_status_codes() {
        assert_eq!(parse_battery_status(&[50, 20, 1]).unwrap().charging, ChargingState::Charging);
        assert_eq!(parse_battery_status(&[90, 50, 2]).unwrap().charging, ChargingState::Charging);
        assert_eq!(parse_battery_status(&[100, 90, 3]).unwrap().charging, ChargingState::Full);
        assert_eq!(parse_battery_status(&[30, 20, 4]).unwrap().charging, ChargingState::Charging);
        assert_eq!(parse_battery_status(&[30, 20, 6]).unwrap().charging, ChargingState::Error);
    }

    #[test]
    fn battery_status_rejects_short_or_bogus_payload() {
        assert_eq!(parse_battery_status(&[50, 20]), None);
        assert_eq!(parse_battery_status(&[101, 20, 0]), None);
    }

    #[test]
    fn battery_features_read_with_their_own_function() {
        assert_eq!(BatteryFeature::Unified(8).read_func(), 1);
        assert_eq!(BatteryFeature::Status(8).read_func(), 0);
    }

    #[test]
    fn battery_event_has_swid_zero() {
        let m = parse(&hex("11 02 08 00 0e 02 00 00 00 00 00 00 00 00 00 00 00 00 00 00")).unwrap();
        assert_eq!(m, Message::Event { dev: 2, feat_idx: 8, func: 0, data: hex("0e 02 00 00 00 00 00 00 00 00 00 00 00 00 00 00") });
    }

    #[test]
    fn parses_hidpp10_error_for_unpaired_slot() {
        // probe: index 1 has no device
        let m = parse(&hex("10 01 8f 00 0a 09 00")).unwrap();
        assert_eq!(m, Message::Error10 { dev: 1, feat_idx: 0, func: 0, swid: SWID, code: 9 });
    }

    #[test]
    fn parses_hidpp20_error() {
        let m = parse(&hex("11 02 ff 08 1a 02 00 00 00 00 00 00 00 00 00 00 00 00 00 00")).unwrap();
        assert_eq!(m, Message::Error20 { dev: 2, feat_idx: 8, func: 1, swid: SWID, code: 2 });
    }

    #[test]
    fn parses_hidpp20_error_in_short_report() {
        let m = parse(&hex("10 02 ff 08 1a 02 00")).unwrap();
        assert_eq!(m, Message::Error20 { dev: 2, feat_idx: 8, func: 1, swid: SWID, code: 2 });
    }

    #[test]
    fn parses_connection_notifications() {
        // Hand-written from Solaar's format: flags & 0x40 => link not established.
        assert_eq!(parse(&hex("10 02 41 10 01 82 b0")), Some(Message::Connection { dev: 2, linked: true }));
        assert_eq!(parse(&hex("10 02 41 10 41 82 b0")), Some(Message::Connection { dev: 2, linked: false }));
    }

    #[test]
    fn receiver_register_reply_is_other() {
        assert_eq!(parse(&hex("10 ff 81 00 00 09 00")), Some(Message::Other));
    }

    #[test]
    fn rejects_short_and_unknown_buffers() {
        assert_eq!(parse(&[]), None);
        assert_eq!(parse(&hex("10 02 08")), None);
        assert_eq!(parse(&hex("11 02 08 1a 5f")), None);
        assert_eq!(parse(&hex("20 02 08 1a 5f 00 00")), None);
    }

    #[test]
    fn device_kind_from_type() {
        assert_eq!(DeviceKind::from_type(0), DeviceKind::Keyboard);
        assert_eq!(DeviceKind::from_type(3), DeviceKind::Mouse);
        assert_eq!(DeviceKind::from_type(5), DeviceKind::Mouse);
        assert_eq!(DeviceKind::from_type(7), DeviceKind::Other);
    }
}
