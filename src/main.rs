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
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    if std::env::args().any(|a| a == "--once") {
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
