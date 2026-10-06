//! HID++ transports over hidapi: Bolt receivers and directly connected BLE devices.

use std::collections::VecDeque;
use std::ffi::{CStr, CString};
use std::io;
use std::time::{Duration, Instant};

use hidapi::{HidApi, HidDevice};
use log::debug;

use crate::hidpp::{
    self, BatteryFeature, BatteryStatus, DeviceKind, Message, FEATURE_BATTERY_STATUS, FEATURE_DEVICE_NAME,
    FEATURE_UNIFIED_BATTERY, SWID,
};
use crate::state::{Device, DeviceKey};

const VENDOR_LOGITECH: u16 = 0x046D;
const PRODUCT_BOLT: u16 = 0xC548;
/// HID++ vendor usage page on receivers.
const USAGE_PAGE_RECEIVER: u16 = 0xFF00;
/// HID++ vendor usage page on Bluetooth LE devices.
const USAGE_PAGE_BLE: u16 = 0xFF43;
/// HID++ collection usages on the vendor page: short (0x10) and long (0x11) reports.
pub const USAGE_SHORT: u16 = 0x0001;
pub const USAGE_LONG: u16 = 0x0002;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
/// Read slice per handle when a transport has several (Windows collections).
const READ_SLICE: Duration = Duration::from_millis(10);
/// HID++ 1.0 set-register 0x00: enable wireless + software-present notifications.
const ENABLE_NOTIFICATIONS: [u8; 7] = [0x10, 0xFF, 0x80, 0x00, 0x00, 0x09, 0x00];
const RECEIVER_INDEXES: [u8; 6] = [1, 2, 3, 4, 5, 6];
const DIRECT_INDEXES: [u8; 1] = [0xFF];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Bolt receiver: devices in slots 1-6, connection notifications.
    Receiver,
    /// Device on its own link (BLE): index 0xFF, long reports only.
    Direct,
}

#[derive(Debug)]
pub enum ReqError {
    Io(io::Error),
    Timeout,
    /// HID++ error code from device or receiver (e.g. device asleep).
    Device(u8),
    Malformed,
}

impl From<io::Error> for ReqError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl std::fmt::Display for ReqError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Timeout => f.write_str("no reply"),
            Self::Device(code) => write!(f, "HID++ error {code:#04x}"),
            Self::Malformed => f.write_str("malformed reply"),
        }
    }
}

/// Result of querying one paired device.
#[derive(Debug)]
pub struct Probe {
    pub device: Device,
    /// Battery feature, if the device has one we can read.
    pub battery_feature: Option<BatteryFeature>,
}

/// Which kind of transport a HID interface is, if any.
pub fn classify(vendor_id: u16, product_id: u16, usage_page: u16) -> Option<Kind> {
    match (vendor_id, product_id, usage_page) {
        (VENDOR_LOGITECH, PRODUCT_BOLT, USAGE_PAGE_RECEIVER) => Some(Kind::Receiver),
        (VENDOR_LOGITECH, _, USAGE_PAGE_BLE) => Some(Kind::Direct),
        _ => None,
    }
}

/// Key shared by all HID collections of one interface. Windows lists each
/// top-level collection as its own path
/// (`\\?\HID#<hardware id>&Col01#<instance>&0000#{class}`): strip the
/// collection from the hardware id and the instance's last part. Paths
/// without that shape (Linux hidraw) are kept as they are.
pub fn group_key(path: &str) -> String {
    let mut parts: Vec<String> = path.split('#').map(String::from).collect();
    if parts.len() < 3 {
        return path.into();
    }
    parts[1] = strip_collection(&parts[1]);
    if let Some((head, _)) = parts[2].rsplit_once('&') {
        parts[2] = head.into();
    }
    parts.join("#")
}

/// Remove a `&ColNN` segment, matched case-insensitively.
fn strip_collection(s: &str) -> String {
    let Some(at) = s.to_ascii_lowercase().find("&col") else { return s.into() };
    let digits = s[at + 4..].bytes().take_while(u8::is_ascii_hexdigit).count();
    format!("{}{}", &s[..at], &s[at + 4 + digits..])
}

/// One HID collection of a transport and its usage on the vendor page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidPath {
    pub path: CString,
    pub usage: u16,
}

/// One transport to open: every HID collection of it, in discovery order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Grouped path plus vendor/product, so a reused Linux hidraw path that
    /// now belongs to another device is a different transport.
    pub id: String,
    pub kind: Kind,
    pub paths: Vec<HidPath>,
}

/// Group HID interfaces `(vendor, product, usage page, usage, path)` into transports.
pub fn candidates<'a>(infos: impl IntoIterator<Item = (u16, u16, u16, u16, &'a CStr)>) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    for (vid, pid, page, usage, path) in infos {
        let Some(kind) = classify(vid, pid, page) else {
            if vid == VENDOR_LOGITECH {
                debug!("skipping Logitech interface {pid:04x} page {page:04x} at {}", path.to_string_lossy());
            }
            continue;
        };
        let id = format!("{}|{vid:04x}:{pid:04x}", group_key(&path.to_string_lossy()));
        let hid_path = HidPath { path: path.into(), usage };
        match out.iter_mut().find(|c| c.id == id) {
            Some(c) if c.paths.iter().any(|p| p.path.as_c_str() == path) => {}
            Some(c) => c.paths.push(hid_path),
            None => out.push(Candidate { id, kind, paths: vec![hid_path] }),
        }
    }
    out
}

/// Transports among the HID devices `api` currently knows.
pub fn discover(api: &HidApi) -> Vec<Candidate> {
    candidates(api.device_list().map(|d| (d.vendor_id(), d.product_id(), d.usage_page(), d.usage(), d.path())))
}

/// Order to try handles in for a report of `len` bytes: the collection whose
/// usage matches the report type first, the rest as a fallback.
fn write_order(usages: &[u16], len: usize) -> Vec<usize> {
    let wanted = if len == hidpp::SHORT_LEN { USAGE_SHORT } else { USAGE_LONG };
    let (mut first, rest): (Vec<usize>, Vec<usize>) = (0..usages.len()).partition(|&i| usages[i] == wanted);
    first.extend(rest);
    first
}

/// Device indexes to probe on a transport.
pub fn indexes_for(kind: Kind) -> &'static [u8] {
    match kind {
        Kind::Receiver => &RECEIVER_INDEXES,
        Kind::Direct => &DIRECT_INDEXES,
    }
}

/// Request framing per transport: BLE links only take long reports.
fn encode_request(kind: Kind, dev: u8, feat_idx: u8, func: u8, params: &[u8]) -> Vec<u8> {
    match kind {
        Kind::Receiver => hidpp::request(dev, feat_idx, func, params).to_vec(),
        Kind::Direct => hidpp::request_long(dev, feat_idx, func, params).to_vec(),
    }
}

/// Append a DEVICE_NAME chunk; `total` is the full name length. Returns false when done.
fn append_name(name: &mut Vec<u8>, chunk: &[u8], total: usize) -> bool {
    let want = total.saturating_sub(name.len()).min(chunk.len());
    let part = &chunk[..want];
    let end = part.iter().position(|&b| b == 0);
    name.extend_from_slice(&part[..end.unwrap_or(want)]);
    end.is_none() && want > 0 && name.len() < total
}

fn hid_err(e: hidapi::HidError) -> io::Error {
    io::Error::other(e)
}

pub struct Transport {
    id: String,
    kind: Kind,
    /// One handle per HID collection; on Linux there is just one.
    handles: Vec<HidDevice>,
    /// Vendor-page usage of each handle, for choosing where to write.
    usages: Vec<u16>,
    /// Unsolicited messages read while waiting for a reply.
    pending: VecDeque<Message>,
}

impl Transport {
    pub fn open(api: &HidApi, c: &Candidate) -> io::Result<Self> {
        let handles = c.paths.iter().map(|p| api.open_path(&p.path).map_err(hid_err)).collect::<io::Result<_>>()?;
        let usages = c.paths.iter().map(|p| p.usage).collect();
        let mut t = Self { id: c.id.clone(), kind: c.kind, handles, usages, pending: VecDeque::new() };
        if t.kind == Kind::Receiver {
            t.write(&ENABLE_NOTIFICATIONS)?;
        }
        Ok(t)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn indexes(&self) -> &'static [u8] {
        indexes_for(self.kind)
    }

    pub fn key(&self, index: u8) -> DeviceKey {
        DeviceKey { transport: self.id.clone(), index }
    }

    /// Write on the collection for this report type, falling back to the others.
    fn write(&mut self, report: &[u8]) -> io::Result<()> {
        let mut last = None;
        for i in write_order(&self.usages, report.len()) {
            match self.handles[i].write(report) {
                Ok(_) => return Ok(()),
                Err(e) => last = Some(e),
            }
        }
        Err(last.map_or_else(|| io::ErrorKind::NotConnected.into(), hid_err))
    }

    /// Wait up to `timeout` for one message from any collection. `Ok(None)` on timeout.
    fn read_message(&mut self, timeout: Duration) -> io::Result<Option<Message>> {
        let deadline = Instant::now() + timeout;
        let mut buf = [0u8; 64];
        loop {
            for h in &self.handles {
                let left = deadline.saturating_duration_since(Instant::now());
                let slice = if self.handles.len() == 1 { left } else { left.min(READ_SLICE) };
                let ms = slice.as_millis().min(i32::MAX as u128) as i32;
                let len = h.read_timeout(&mut buf, ms).map_err(hid_err)?;
                if len > 0 {
                    return Ok(hidpp::parse(&buf[..len]));
                }
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
        }
    }

    pub fn request(&mut self, dev: u8, feat_idx: u8, func: u8, params: &[u8]) -> Result<Vec<u8>, ReqError> {
        self.write(&encode_request(self.kind, dev, feat_idx, func, params))?;
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(ReqError::Timeout);
            }
            let Some(msg) = self.read_message(left)? else { continue };
            match msg {
                Message::Reply { dev: d, feat_idx: i, func: f, swid: SWID, data }
                    if (d, i, f) == (dev, feat_idx, func) =>
                {
                    return Ok(data);
                }
                Message::Error20 { dev: d, feat_idx: i, func: f, swid: SWID, code }
                | Message::Error10 { dev: d, feat_idx: i, func: f, swid: SWID, code }
                    if (d, i, f) == (dev, feat_idx, func) =>
                {
                    return Err(ReqError::Device(code));
                }
                Message::Event { .. } | Message::Connection { .. } => self.pending.push_back(msg),
                other => debug!("dropping {other:?}"),
            }
        }
    }

    /// Next unsolicited message (battery event, connection change), or `None` on timeout.
    pub fn next_event(&mut self, timeout: Duration) -> io::Result<Option<Message>> {
        if let Some(msg) = self.pending.pop_front() {
            return Ok(Some(msg));
        }
        match self.read_message(timeout)? {
            Some(msg @ (Message::Event { .. } | Message::Connection { .. })) => Ok(Some(msg)),
            _ => Ok(None),
        }
    }

    fn feature_index(&mut self, dev: u8, feature: u16) -> Result<Option<u8>, ReqError> {
        let data = self.request(dev, 0, 0, &feature.to_be_bytes())?;
        Ok(hidpp::parse_feature_index(&data))
    }

    pub fn read_battery(&mut self, dev: u8, feature: BatteryFeature) -> Result<BatteryStatus, ReqError> {
        let data = self.request(dev, feature.index(), feature.read_func(), &[])?;
        feature.parse(&data).ok_or(ReqError::Malformed)
    }

    /// UNIFIED_BATTERY if the device has it, else BATTERY_STATUS.
    fn battery_feature(&mut self, dev: u8) -> Result<Option<BatteryFeature>, ReqError> {
        if let Some(idx) = self.feature_index(dev, FEATURE_UNIFIED_BATTERY)? {
            return Ok(Some(BatteryFeature::Unified(idx)));
        }
        Ok(self.feature_index(dev, FEATURE_BATTERY_STATUS)?.map(BatteryFeature::Status))
    }

    fn read_name(&mut self, dev: u8, name_idx: u8) -> Result<(String, DeviceKind), ReqError> {
        let total = *self.request(dev, name_idx, 0, &[])?.first().ok_or(ReqError::Malformed)? as usize;
        let mut name = Vec::with_capacity(total);
        while name.len() < total {
            let chunk = self.request(dev, name_idx, 1, &[name.len() as u8])?;
            if !append_name(&mut name, &chunk, total) {
                break;
            }
        }
        let kind = self.request(dev, name_idx, 2, &[])?;
        let kind = DeviceKind::from_type(*kind.first().ok_or(ReqError::Malformed)?);
        Ok((String::from_utf8_lossy(&name).into_owned(), kind))
    }

    /// Query a paired device's name, kind and battery.
    pub fn probe_device(&mut self, dev: u8) -> Result<Probe, ReqError> {
        let (name, kind) = match self.feature_index(dev, FEATURE_DEVICE_NAME)? {
            Some(idx) => self.read_name(dev, idx)?,
            None => (format!("Device {dev}"), DeviceKind::Other),
        };
        let battery_feature = self.battery_feature(dev)?;
        let battery = match battery_feature {
            Some(feature) => Some(self.read_battery(dev, feature)?),
            None => None,
        };
        Ok(Probe { device: Device { key: self.key(dev), name, kind, battery, online: true }, battery_feature })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    const LOGI: u16 = 0x046D;

    fn info(vid: u16, pid: u16, page: u16, path: &str) -> (u16, u16, u16, u16, CString) {
        info_usage(vid, pid, page, 1, path)
    }

    fn info_usage(vid: u16, pid: u16, page: u16, usage: u16, path: &str) -> (u16, u16, u16, u16, CString) {
        (vid, pid, page, usage, CString::new(path).unwrap())
    }

    fn found(list: &[(u16, u16, u16, u16, CString)]) -> Vec<Candidate> {
        candidates(list.iter().map(|(v, p, pg, u, path)| (*v, *p, *pg, *u, path.as_c_str())))
    }

    #[test]
    fn classifies_bolt_hidpp_interface_as_receiver() {
        assert_eq!(classify(LOGI, 0xC548, 0xFF00), Some(Kind::Receiver));
    }

    #[test]
    fn ignores_bolt_non_hidpp_interfaces() {
        assert_eq!(classify(LOGI, 0xC548, 0x0001), None);
        assert_eq!(classify(LOGI, 0xC548, 0x000C), None);
    }

    #[test]
    fn ignores_unifying_receiver() {
        assert_eq!(classify(LOGI, 0xC52B, 0xFF00), None);
    }

    #[test]
    fn classifies_ble_hidpp_interface_as_direct() {
        assert_eq!(classify(LOGI, 0xB034, 0xFF43), Some(Kind::Direct));
    }

    #[test]
    fn ignores_other_vendors() {
        assert_eq!(classify(0x044F, 0xC548, 0xFF00), None);
        assert_eq!(classify(0x044F, 0xB034, 0xFF43), None);
    }

    const WIN_COL1: &str = r"\\?\HID#VID_046D&PID_C548&MI_02&Col01#8&2a1b3c4d&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}";
    const WIN_COL2: &str = r"\\?\HID#VID_046D&PID_C548&MI_02&Col02#8&2a1b3c4d&0&0001#{4d1e55b2-f16f-11cf-88cb-001111000030}";

    #[test]
    fn windows_collections_share_a_group_key() {
        assert_eq!(group_key(WIN_COL1), group_key(WIN_COL2));
        assert!(!group_key(WIN_COL1).to_ascii_lowercase().contains("&col"));
    }

    #[test]
    fn windows_interfaces_keep_distinct_group_keys() {
        let other = WIN_COL1.replace("MI_02", "MI_01");
        assert_ne!(group_key(WIN_COL1), group_key(&other));
        let other_instance = WIN_COL1.replace("2a1b3c4d", "11112222");
        assert_ne!(group_key(WIN_COL1), group_key(&other_instance));
    }

    #[test]
    fn linux_paths_are_their_own_group_key() {
        assert_eq!(group_key("/dev/hidraw3"), "/dev/hidraw3");
    }

    #[test]
    fn windows_collections_form_one_candidate() {
        let c = found(&[info(LOGI, 0xC548, 0xFF00, WIN_COL1), info(LOGI, 0xC548, 0xFF00, WIN_COL2)]);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].kind, Kind::Receiver);
        assert_eq!(c[0].paths.len(), 2);
    }

    #[test]
    fn repeated_linux_path_is_opened_once() {
        let c = found(&[info(LOGI, 0xC548, 0xFF00, "/dev/hidraw3"), info(LOGI, 0xC548, 0xFF00, "/dev/hidraw3")]);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].paths.len(), 1);
        assert!(c[0].id.starts_with("/dev/hidraw3"));
    }

    #[test]
    fn receiver_and_ble_device_are_separate_candidates() {
        let c = found(&[
            info(LOGI, 0xC548, 0x0001, "/dev/hidraw1"),
            info(LOGI, 0xC548, 0xFF00, "/dev/hidraw3"),
            info(LOGI, 0xB034, 0xFF43, "/dev/hidraw7"),
            info(0x044F, 0xB108, 0xFF00, "/dev/hidraw9"),
        ]);
        let kinds: Vec<Kind> = c.iter().map(|c| c.kind).collect();
        assert_eq!(kinds, [Kind::Receiver, Kind::Direct]);
        assert!(c[0].id.starts_with("/dev/hidraw3") && c[1].id.starts_with("/dev/hidraw7"));
    }

    #[test]
    fn receiver_probes_six_slots_and_direct_device_one() {
        assert_eq!(indexes_for(Kind::Receiver), [1, 2, 3, 4, 5, 6]);
        assert_eq!(indexes_for(Kind::Direct), [0xFF]);
    }

    #[test]
    fn direct_requests_use_long_reports() {
        let req = encode_request(Kind::Direct, 0xFF, 2, 1, &[]);
        assert_eq!(req.len(), hidpp::LONG_LEN);
        assert_eq!(req[0], hidpp::REPORT_LONG);
        let req = encode_request(Kind::Receiver, 1, 2, 1, &[]);
        assert_eq!(req.len(), hidpp::SHORT_LEN);
        assert_eq!(req[0], hidpp::REPORT_SHORT);
    }

    #[test]
    fn assembles_name_from_chunks() {
        let mut name = Vec::new();
        let mut chunk = *b"MX Master 3S\0\0\0\0";
        assert!(!append_name(&mut name, &chunk, 12));
        assert_eq!(name, b"MX Master 3S");

        // 20-char name over two 16-byte chunks
        let mut name = Vec::new();
        chunk = *b"Logitech Wireles";
        assert!(append_name(&mut name, &chunk, 20));
        assert!(!append_name(&mut name, b"s Mo\0\0\0\0\0\0\0\0\0\0\0\0", 20));
        assert_eq!(name, b"Logitech Wireless Mo");
    }

    #[test]
    fn name_stops_at_nul_or_empty_chunk() {
        let mut name = Vec::new();
        assert!(!append_name(&mut name, b"MX\0junk", 9));
        assert_eq!(name, b"MX");
        let mut name = Vec::new();
        assert!(!append_name(&mut name, b"", 9));
    }

    #[test]
    fn reused_path_for_another_product_is_another_transport() {
        let receiver = found(&[info(LOGI, 0xC548, 0xFF00, "/dev/hidraw3")]);
        let mouse = found(&[info(LOGI, 0xB034, 0xFF43, "/dev/hidraw3")]);
        assert_ne!(receiver[0].id, mouse[0].id);
    }

    #[test]
    fn candidate_records_each_collection_usage() {
        let c = found(&[info_usage(LOGI, 0xC548, 0xFF00, 2, WIN_COL2), info_usage(LOGI, 0xC548, 0xFF00, 1, WIN_COL1)]);
        let usages: Vec<u16> = c[0].paths.iter().map(|p| p.usage).collect();
        assert_eq!(usages, [2, 1]);
    }

    #[test]
    fn writes_go_to_the_collection_for_the_report_length() {
        assert_eq!(write_order(&[USAGE_LONG, USAGE_SHORT], hidpp::SHORT_LEN), [1, 0]);
        assert_eq!(write_order(&[USAGE_LONG, USAGE_SHORT], hidpp::LONG_LEN), [0, 1]);
        assert_eq!(write_order(&[USAGE_SHORT], hidpp::LONG_LEN), [0]);
        assert_eq!(write_order(&[7, 9], hidpp::SHORT_LEN), [0, 1]);
    }
}
