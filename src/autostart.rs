//! Start at login: an XDG autostart entry on Linux, an HKCU Run value on Windows.

#[cfg(target_os = "linux")]
pub use self::linux::*;
#[cfg(windows)]
pub use self::windows::*;

#[cfg(target_os = "linux")]
mod linux {
    use std::io;
    use std::path::{Path, PathBuf};

    /// Entry file name; same as `just install` writes.
    pub(super) const ENTRY: &str = "logi-battery-tray.desktop";
    const DESKTOP: &str = include_str!("../packaging/logi-battery-tray.desktop");

    fn autostart_dir() -> PathBuf {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_default()
            .join("autostart")
    }

    pub fn is_enabled_in(dir: &Path) -> bool {
        dir.join(ENTRY).is_file()
    }

    pub fn set_enabled_in(dir: &Path, on: bool) -> io::Result<()> {
        let entry = dir.join(ENTRY);
        if on {
            std::fs::create_dir_all(dir)?;
            return std::fs::write(entry, DESKTOP);
        }
        match std::fs::remove_file(entry) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }

    pub fn is_enabled() -> bool {
        is_enabled_in(&autostart_dir())
    }

    pub fn set_enabled(on: bool) -> io::Result<()> {
        set_enabled_in(&autostart_dir(), on)
    }
}

#[cfg(windows)]
mod windows {
    use std::io;

    use windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND;
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
    };

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    /// Value name; same as `just install` writes.
    const VALUE: &str = "LogiBatteryTray";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    fn check(code: u32) -> io::Result<()> {
        if code == 0 { Ok(()) } else { Err(io::Error::from_raw_os_error(code as i32)) }
    }

    pub fn is_enabled() -> bool {
        let (key, value) = (wide(RUN_KEY), wide(VALUE));
        let code = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        code == 0
    }

    pub fn set_enabled(on: bool) -> io::Result<()> {
        let (key, value) = (wide(RUN_KEY), wide(VALUE));
        if !on {
            let code = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr()) };
            return if code == ERROR_FILE_NOT_FOUND { Ok(()) } else { check(code) };
        }
        let exe = std::env::current_exe()?;
        let data = wide(&format!("\"{}\"", exe.display()));
        let bytes = (data.len() * 2) as u32;
        let code = unsafe {
            RegSetKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr(), REG_SZ, data.as_ptr().cast(), bytes)
        };
        check(code)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("logi-battery-tray-autostart-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn enabling_writes_the_entry_and_disabling_removes_it() {
        let dir = temp_dir("toggle");
        assert!(!is_enabled_in(&dir));
        set_enabled_in(&dir, true).unwrap();
        assert!(is_enabled_in(&dir));
        let entry = std::fs::read_to_string(dir.join(ENTRY)).unwrap();
        assert!(entry.contains("Exec=logi-battery-tray"));
        set_enabled_in(&dir, false).unwrap();
        assert!(!is_enabled_in(&dir));
    }

    #[test]
    fn toggling_twice_in_a_row_is_harmless() {
        let dir = temp_dir("twice");
        set_enabled_in(&dir, true).unwrap();
        set_enabled_in(&dir, true).unwrap();
        assert!(is_enabled_in(&dir));
        set_enabled_in(&dir, false).unwrap();
        set_enabled_in(&dir, false).unwrap();
        assert!(!is_enabled_in(&dir));
    }

    #[test]
    fn missing_autostart_directory_is_created() {
        let dir = temp_dir("missing").join("nested");
        set_enabled_in(&dir, true).unwrap();
        assert!(is_enabled_in(&dir));
    }
}
