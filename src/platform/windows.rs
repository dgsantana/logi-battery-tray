//! Windows: notification-area icon and toasts. Filled in by the Windows tray task.

use std::process::ExitCode;
use std::sync::mpsc::Sender;

use log::{error, info};

use super::Publisher;
use crate::state::Alert;
use crate::tray::Cmd;

pub fn run(_cmd_tx: Sender<Cmd>, _worker: Box<dyn FnOnce(Publisher) + Send>) -> ExitCode {
    error!("the Windows tray is not implemented yet");
    ExitCode::FAILURE
}

pub fn notify(alert: &Alert) {
    info!("low battery: {} {}%", alert.name, alert.percent);
}
