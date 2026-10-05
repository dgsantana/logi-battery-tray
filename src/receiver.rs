//! Logi Bolt receiver access over /dev/hidraw.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use log::debug;

use crate::hidpp::{
    self, BatteryStatus, DeviceKind, Message, FEATURE_DEVICE_NAME, FEATURE_UNIFIED_BATTERY, SWID,
};
use crate::state::{Device, DeviceKey};

const VENDOR_LOGITECH: &str = "0000046D";
const PRODUCT_BOLT: &str = "0000C548";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
/// HID++ 1.0 set-register 0x00: enable wireless + software-present notifications.
const ENABLE_NOTIFICATIONS: [u8; 7] = [0x10, 0xFF, 0x80, 0x00, 0x00, 0x09, 0x00];
pub const DEVICE_INDEXES: std::ops::RangeInclusive<u8> = 1..=6;

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
    /// Feature index of UNIFIED_BATTERY, if the device has it.
    pub battery_idx: Option<u8>,
}

/// True when a hidraw uevent belongs to a Logi Bolt receiver.
fn matches_receiver(uevent: &str) -> bool {
    uevent
        .lines()
        .filter_map(|l| l.strip_prefix("HID_ID="))
        .any(|id| id.split(':').skip(1).eq([VENDOR_LOGITECH, PRODUCT_BOLT]))
}

/// True when a report descriptor declares the HID++ vendor page with short report 0x10.
fn is_hidpp_descriptor(desc: &[u8]) -> bool {
    let has = |needle: &[u8]| desc.windows(needle.len()).any(|w| w == needle);
    // Usage Page (0xFF00), Report ID (0x10)
    has(&[0x06, 0x00, 0xFF]) && has(&[0x85, hidpp::REPORT_SHORT])
}

/// Append a DEVICE_NAME chunk; `total` is the full name length. Returns false when done.
fn append_name(name: &mut Vec<u8>, chunk: &[u8], total: usize) -> bool {
    let want = total.saturating_sub(name.len()).min(chunk.len());
    let part = &chunk[..want];
    let end = part.iter().position(|&b| b == 0);
    name.extend_from_slice(&part[..end.unwrap_or(want)]);
    end.is_none() && want > 0 && name.len() < total
}

/// Locate the receiver's HID++ hidraw node.
pub fn find() -> Option<PathBuf> {
    let entries = fs::read_dir("/sys/class/hidraw").ok()?;
    for entry in entries.flatten() {
        let dev = entry.path().join("device");
        let Ok(uevent) = fs::read_to_string(dev.join("uevent")) else { continue };
        let Ok(desc) = fs::read(dev.join("report_descriptor")) else { continue };
        if matches_receiver(&uevent) && is_hidpp_descriptor(&desc) {
            return Some(Path::new("/dev").join(entry.file_name()));
        }
    }
    None
}

pub struct Receiver {
    file: File,
    /// Transport id: the hidraw path.
    id: String,
    /// Unsolicited messages read while waiting for a reply.
    pending: VecDeque<Message>,
}

impl Receiver {
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        file.write_all(&ENABLE_NOTIFICATIONS)?;
        Ok(Self { file, id: path.display().to_string(), pending: VecDeque::new() })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn key(&self, index: u8) -> DeviceKey {
        DeviceKey { transport: self.id.clone(), index }
    }

    /// Wait up to `timeout` for one message. `Ok(None)` on timeout.
    fn read_message(&mut self, timeout: Duration) -> io::Result<Option<Message>> {
        let mut pfd = libc::pollfd { fd: self.file.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        let n = unsafe { libc::poll(&mut pfd, 1, ms) };
        if n < 0 {
            let err = io::Error::last_os_error();
            return if err.kind() == io::ErrorKind::Interrupted { Ok(None) } else { Err(err) };
        }
        if n == 0 {
            return Ok(None);
        }
        if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "receiver went away"));
        }
        let mut buf = [0u8; 64];
        let len = self.file.read(&mut buf)?;
        if len == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        Ok(hidpp::parse(&buf[..len]))
    }

    pub fn request(&mut self, dev: u8, feat_idx: u8, func: u8, params: &[u8]) -> Result<Vec<u8>, ReqError> {
        self.file.write_all(&hidpp::request(dev, feat_idx, func, params))?;
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

    pub fn read_battery(&mut self, dev: u8, battery_idx: u8) -> Result<BatteryStatus, ReqError> {
        let data = self.request(dev, battery_idx, 1, &[])?;
        hidpp::parse_battery(&data).ok_or(ReqError::Malformed)
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
        let battery_idx = self.feature_index(dev, FEATURE_UNIFIED_BATTERY)?;
        let battery = match battery_idx {
            Some(idx) => Some(self.read_battery(dev, idx)?),
            None => None,
        };
        Ok(Probe { device: Device { key: self.key(dev), name, kind, battery, online: true }, battery_idx })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_bolt_uevent() {
        let bolt = "DRIVER=hid-generic\nHID_ID=0003:0000046D:0000C548\nHID_NAME=Logitech USB Receiver\n";
        assert!(matches_receiver(bolt));
        assert!(!matches_receiver("HID_ID=0003:0000046D:0000C52B\n"));
        assert!(!matches_receiver("HID_ID=0003:0000044F:0000B108\n"));
        assert!(!matches_receiver(""));
    }

    #[test]
    fn detects_hidpp_descriptor() {
        // prefix of hidraw3 on this machine
        let hidpp = [0x06, 0x00, 0xff, 0x09, 0x01, 0xa1, 0x01, 0x85, 0x10, 0x95, 0x06];
        assert!(is_hidpp_descriptor(&hidpp));
        // hidraw1: keyboard interface
        let kbd = [0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x95, 0x08];
        assert!(!is_hidpp_descriptor(&kbd));
        // vendor page without report 0x10
        assert!(!is_hidpp_descriptor(&[0x06, 0x00, 0xff, 0x85, 0x20]));
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
}
