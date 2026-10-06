//! Per-OS UI: tray icon, menu, notifications and the thread model around them.

use crate::tray::Snapshot;

/// Hands a fresh snapshot to the UI; called from the worker thread.
pub type Publisher = Box<dyn Fn(Snapshot) + Send>;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use self::linux::{attach_console, log_file, notify, run};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::{attach_console, log_file, notify, run};
