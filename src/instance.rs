//! Single-instance guard: a second tray process exits instead of adding an icon.

use std::io;

/// Startup decision: run unless another instance holds the guard. A failing
/// check never blocks startup.
pub fn should_run<T>(acquired: &io::Result<Option<T>>) -> bool {
    !matches!(acquired, Ok(None))
}

#[cfg(target_os = "linux")]
pub use self::linux::*;
#[cfg(windows)]
pub use self::windows::*;

#[cfg(target_os = "linux")]
mod linux {
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::fd::AsRawFd;
    use std::path::{Path, PathBuf};

    const LOCK_FILE: &str = "logi-battery-tray.lock";

    /// Held while this process is the only instance; the lock goes away with
    /// the file descriptor, including on a crash.
    #[derive(Debug)]
    pub struct Guard(#[allow(dead_code)] File);

    /// Take an exclusive lock on the lock file in `dir`. `Ok(None)` when
    /// another process holds it.
    pub fn acquire_in(dir: &Path) -> io::Result<Option<Guard>> {
        let file = OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(LOCK_FILE))?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Some(Guard(file)));
        }
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::WouldBlock { Ok(None) } else { Err(err) }
    }

    pub fn acquire() -> io::Result<Option<Guard>> {
        let dir = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
        acquire_in(&dir)
    }
}

#[cfg(windows)]
mod windows {
    use std::io;

    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    /// Held while this process is the only instance; Windows releases the
    /// mutex when the process exits, including on a crash.
    #[derive(Debug)]
    pub struct Guard(HANDLE);

    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    /// Create the named session mutex. `Ok(None)` when it already exists.
    pub fn acquire() -> io::Result<Option<Guard>> {
        let name: Vec<u16> = "Local\\logi-battery-tray".encode_utf16().chain([0]).collect();
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe { CloseHandle(handle) };
            return Ok(None);
        }
        Ok(Some(Guard(handle)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("logi-battery-tray-test-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn second_acquire_fails_while_first_is_held() {
        let dir = temp_dir("held");
        let first = acquire_in(&dir).unwrap();
        assert!(first.is_some());
        assert!(acquire_in(&dir).unwrap().is_none());
    }

    #[test]
    fn acquire_succeeds_again_after_release() {
        let dir = temp_dir("release");
        let first = acquire_in(&dir).unwrap();
        drop(first);
        assert!(acquire_in(&dir).unwrap().is_some());
    }

    #[test]
    fn missing_directory_is_an_error() {
        let dir = temp_dir("missing").join("does-not-exist");
        assert!(acquire_in(&dir).is_err());
    }

    #[test]
    fn startup_runs_unless_another_instance_holds_the_guard() {
        assert!(should_run(&Ok(Some(()))));
        assert!(!should_run::<()>(&Ok(None)));
        assert!(should_run::<()>(&Err(std::io::Error::other("no runtime dir"))));
    }
}
