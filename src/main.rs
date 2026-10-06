#![cfg_attr(windows, windows_subsystem = "windows")]

mod autostart;
mod hidpp;
mod icon;
mod instance;
mod platform;
mod state;
mod transport;
mod tray;
mod worker;

use std::process::ExitCode;
use std::sync::mpsc;

use log::{info, warn};

fn main() -> ExitCode {
    let once = std::env::args().any(|a| a == "--once");
    let mut logger = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"));
    if let Some(file) = platform::log_file().filter(|_| !once) {
        logger.target(env_logger::Target::Pipe(Box::new(file)));
    }
    logger.init();
    if once {
        platform::attach_console();
        return worker::run_once();
    }

    let guard = instance::acquire();
    if let Err(e) = &guard {
        warn!("single-instance check failed, starting anyway: {e}");
    }
    if !instance::should_run(&guard) {
        info!("already running");
        return ExitCode::SUCCESS;
    }

    let (tx, rx) = mpsc::channel();
    platform::run(tx, Box::new(move |publish| worker::run(&rx, publish, platform::notify)))
}
