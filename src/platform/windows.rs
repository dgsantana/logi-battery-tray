//! Windows: notification-area icon (tray-icon) and toasts (notify-rust).
//!
//! tray-icon needs a Win32 message loop on the thread that owns the icon, so
//! the main thread pumps messages while the worker runs on its own thread and
//! sends snapshots over a channel.

use std::process::ExitCode;
use std::sync::mpsc::{self, Sender};
use std::thread;

use log::{error, info, warn};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, MsgWaitForMultipleObjects, PM_REMOVE, PeekMessageW, QS_ALLINPUT, TranslateMessage,
};

use super::Publisher;
use crate::autostart;
use crate::icon;
use crate::state::Alert;
use crate::tray::{Cmd, Snapshot, TITLE, device_line, summary};

/// AppUserModelID toasts are attributed to; `just install` registers it.
const APP_ID: &str = "dgsantana.LogiBatteryTray";
/// Longest tooltip Windows shows, in UTF-16 units including the terminator.
const TOOLTIP_MAX: usize = 127;
/// Upper bound on one message-loop wait, so snapshots are picked up promptly.
const PUMP_WAIT_MS: u32 = 100;

/// Log file for tray mode, which has no console:
/// `%LOCALAPPDATA%\logi-battery-tray\logi-battery-tray.log`, truncated per start.
pub fn log_file() -> Option<std::fs::File> {
    let dir = std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("logi-battery-tray");
    std::fs::create_dir_all(&dir).ok()?;
    std::fs::File::create(dir.join("logi-battery-tray.log")).ok()
}

/// Send `--once` output to the terminal that started us (the binary has no
/// console of its own).
pub fn attach_console() {
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

/// Fixed menu ids, so a click survives the menu being rebuilt.
const MENU_REFRESH: &str = "refresh";
const MENU_QUIT: &str = "quit";
const MENU_AUTOSTART: &str = "autostart";

struct Ui {
    tray: TrayIcon,
}

impl Ui {
    fn new(snap: &Snapshot) -> Result<Self, tray_icon::Error> {
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(build_menu(snap)))
            .with_icon(render_icon(snap))
            .with_tooltip(tooltip(snap))
            .build()?;
        Ok(Self { tray })
    }

    fn show(&mut self, snap: &Snapshot) {
        self.tray.set_menu(Some(Box::new(build_menu(snap))));
        if let Err(e) = self.tray.set_icon(Some(render_icon(snap))) {
            warn!("cannot update tray icon: {e}");
        }
        if let Err(e) = self.tray.set_tooltip(Some(tooltip(snap))) {
            warn!("cannot update tooltip: {e}");
        }
    }

}

fn command(id: &MenuId) -> Option<Cmd> {
    match id.as_ref() {
        MENU_REFRESH => Some(Cmd::Refresh),
        MENU_QUIT => Some(Cmd::Quit),
        _ => None,
    }
}

/// Device lines (disabled), then Start at login, Refresh and Quit.
fn build_menu(snap: &Snapshot) -> Menu {
    let menu = Menu::new();
    let lines: Vec<String> = if snap.devices.is_empty() || !snap.present {
        vec![summary(&snap.devices, snap.present)]
    } else {
        snap.devices.iter().map(device_line).collect()
    };
    let login = CheckMenuItem::with_id(MENU_AUTOSTART, "Start at login", true, autostart::is_enabled(), None);
    let refresh = MenuItem::with_id(MENU_REFRESH, "Refresh", true, None);
    let quit = MenuItem::with_id(MENU_QUIT, "Quit", true, None);
    let appended = lines
        .iter()
        .try_for_each(|l| menu.append(&MenuItem::new(l, false, None)))
        .and_then(|()| menu.append(&PredefinedMenuItem::separator()))
        .and_then(|()| menu.append(&login))
        .and_then(|()| menu.append(&refresh))
        .and_then(|()| menu.append(&quit));
    if let Err(e) = appended {
        warn!("cannot build tray menu: {e}");
    }
    menu
}

fn render_icon(snap: &Snapshot) -> Icon {
    let lowest = snap.lowest.filter(|_| snap.present);
    Icon::from_rgba(icon::render(lowest), icon::SIZE, icon::SIZE).expect("icon buffer matches its size")
}

fn tooltip(snap: &Snapshot) -> String {
    let text = format!("{TITLE}\n{}", summary(&snap.devices, snap.present));
    let mut out = String::new();
    for c in text.chars() {
        if out.encode_utf16().count() + c.len_utf16() > TOOLTIP_MAX {
            break;
        }
        out.push(c);
    }
    out
}

fn toggle_autostart() {
    if let Err(e) = autostart::set_enabled(!autostart::is_enabled()) {
        warn!("cannot change start at login: {e}");
    }
}

/// Pump Win32 messages for up to `PUMP_WAIT_MS`.
fn pump_messages() {
    unsafe {
        MsgWaitForMultipleObjects(0, std::ptr::null(), 0, PUMP_WAIT_MS, QS_ALLINPUT);
        let mut msg: MSG = std::mem::zeroed();
        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Show the tray on this thread and run the worker on another until it returns.
pub fn run(cmd_tx: Sender<Cmd>, worker: Box<dyn FnOnce(Publisher) + Send>) -> ExitCode {
    // Hover/click events are unused; without a handler tray-icon queues them
    // in an unbounded channel for the life of the process.
    TrayIconEvent::set_event_handler(Some(|_| {}));
    let (snap_tx, snap_rx) = mpsc::channel::<Snapshot>();
    let worker_thread = thread::spawn(move || {
        worker(Box::new(move |snap| {
            let _ = snap_tx.send(snap);
        }))
    });
    let mut shown = Snapshot { devices: Vec::new(), lowest: None, present: false };
    let mut ui = match Ui::new(&shown) {
        Ok(ui) => ui,
        Err(e) => {
            error!("cannot start tray: {e}");
            let _ = cmd_tx.send(Cmd::Quit);
            let _ = worker_thread.join();
            return ExitCode::FAILURE;
        }
    };
    while !worker_thread.is_finished() {
        pump_messages();
        // Only the newest snapshot matters.
        let latest = snap_rx.try_iter().last();
        if let Some(snap) = latest.filter(|s| *s != shown) {
            ui.show(&snap);
            shown = snap;
        }
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id.as_ref() == MENU_AUTOSTART {
                toggle_autostart();
                // The check mark flips on click; rebuild from the real state.
                ui.show(&shown);
            } else if let Some(cmd) = command(&event.id) {
                let _ = cmd_tx.send(cmd);
            }
        }
    }
    if worker_thread.join().is_err() {
        error!("worker thread panicked");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

pub fn notify(alert: &Alert) {
    info!("low battery: {} {}%", alert.name, alert.percent);
    let shown = notify_rust::Notification::new()
        .app_id(APP_ID)
        .summary(&format!("{} battery low", alert.name))
        .body(&format!("{}% remaining", alert.percent))
        .show();
    if let Err(e) = shown {
        warn!("cannot show notification: {e}");
    }
}
