mod hidpp;
mod receiver;
mod state;
mod tray;

use std::collections::HashMap;
use std::io;
use std::process::ExitCode;
use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};

use ksni::blocking::TrayMethods;
use log::{debug, error, info, warn};

use crate::hidpp::Message;
use crate::receiver::{ReqError, Receiver, DEVICE_INDEXES};
use crate::state::{Alert, State};
use crate::tray::{BatteryTray, Cmd};

const POLL_INTERVAL: Duration = Duration::from_secs(5 * 60);
const REDISCOVER_INTERVAL: Duration = Duration::from_secs(10);
const EVENT_WAIT: Duration = Duration::from_secs(1);

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    if std::env::args().any(|a| a == "--once") {
        return run_once();
    }

    let (tx, rx) = mpsc::channel();
    let tray = BatteryTray { devices: Vec::new(), lowest: None, receiver_present: false, tx };
    let handle = match tray.spawn() {
        Ok(handle) => handle,
        Err(e) => {
            error!("cannot start tray: {e}");
            return ExitCode::FAILURE;
        }
    };
    run(&rx, |state, present| {
        handle.update(|t| {
            t.devices = state.snapshot();
            t.lowest = state.lowest();
            t.receiver_present = present;
        });
    });
    handle.shutdown().wait();
    ExitCode::SUCCESS
}

/// Print every device's battery once and exit.
fn run_once() -> ExitCode {
    let Some(path) = receiver::find() else {
        eprintln!("Receiver not found");
        return ExitCode::FAILURE;
    };
    let mut rcv = match Receiver::open(&path) {
        Ok(rcv) => rcv,
        Err(e) => {
            eprintln!("cannot open {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    let mut state = State::default();
    let mut battery_idx = HashMap::new();
    for dev in DEVICE_INDEXES {
        if let Err(e) = probe(&mut rcv, &mut state, &mut battery_idx, dev) {
            eprintln!("receiver I/O error: {e}");
            return ExitCode::FAILURE;
        }
    }
    println!("{}", tray::summary(&state.snapshot(), true));
    ExitCode::SUCCESS
}

/// Worker loop: (re)discover the receiver, track devices, publish changes until Quit.
fn run(cmds: &mpsc::Receiver<Cmd>, publish: impl Fn(&State, bool)) {
    let mut state = State::default();
    loop {
        let opened = receiver::find().map(|path| (Receiver::open(&path), path));
        let mut rcv = match opened {
            Some((Ok(rcv), path)) => {
                info!("using receiver at {}", path.display());
                rcv
            }
            Some((Err(e), path)) => {
                warn!("cannot open {}: {e}", path.display());
                publish(&state, false);
                if wait_for_retry(cmds) { return } else { continue }
            }
            None => {
                debug!("receiver not found");
                publish(&state, false);
                if wait_for_retry(cmds) { return } else { continue }
            }
        };
        match track(&mut rcv, &mut state, cmds, &publish) {
            Ok(()) => return,
            Err(e) => {
                warn!("lost receiver: {e}");
                publish(&state, false);
            }
        }
    }
}

/// Sleep until rediscovery is due. Returns true on Quit.
fn wait_for_retry(cmds: &mpsc::Receiver<Cmd>) -> bool {
    match cmds.recv_timeout(REDISCOVER_INTERVAL) {
        Ok(Cmd::Quit) | Err(RecvTimeoutError::Disconnected) => true,
        Ok(Cmd::Refresh) | Err(RecvTimeoutError::Timeout) => false,
    }
}

/// Follow one open receiver. `Ok` means Quit was requested; `Err` means the receiver is gone.
fn track(
    rcv: &mut Receiver,
    state: &mut State,
    cmds: &mpsc::Receiver<Cmd>,
    publish: &impl Fn(&State, bool),
) -> io::Result<()> {
    // Feature index of UNIFIED_BATTERY per online device.
    let mut battery_idx: HashMap<u8, u8> = HashMap::new();
    let mut next_poll = Instant::now();
    loop {
        match cmds.try_recv() {
            Ok(Cmd::Quit) | Err(TryRecvError::Disconnected) => return Ok(()),
            Ok(Cmd::Refresh) => next_poll = Instant::now(),
            Err(TryRecvError::Empty) => {}
        }
        if Instant::now() >= next_poll {
            for dev in DEVICE_INDEXES {
                refresh(rcv, state, &mut battery_idx, dev)?;
            }
            publish(state, true);
            next_poll = Instant::now() + POLL_INTERVAL;
        }
        match rcv.next_event(EVENT_WAIT)? {
            Some(Message::Event { dev, feat_idx, func: 0, data }) if battery_idx.get(&dev) == Some(&feat_idx) => {
                let Some(battery) = hidpp::parse_battery(&data) else { continue };
                debug!("battery event dev {dev}: {battery:?}");
                notify(state.set_battery(dev, battery));
                publish(state, true);
            }
            Some(Message::Connection { dev, linked }) => {
                debug!("dev {dev} link {}", if linked { "up" } else { "down" });
                battery_idx.remove(&dev);
                if linked {
                    probe(rcv, state, &mut battery_idx, dev)?;
                } else {
                    state.set_online(dev, false);
                }
                publish(state, true);
            }
            _ => {}
        }
    }
}

/// Re-read a known device's battery, or probe it from scratch.
fn refresh(rcv: &mut Receiver, state: &mut State, battery_idx: &mut HashMap<u8, u8>, dev: u8) -> io::Result<()> {
    let Some(&idx) = battery_idx.get(&dev) else {
        return probe(rcv, state, battery_idx, dev);
    };
    match rcv.read_battery(dev, idx) {
        Ok(battery) => {
            state.set_online(dev, true);
            notify(state.set_battery(dev, battery));
        }
        Err(ReqError::Io(e)) => return Err(e),
        Err(e) => {
            debug!("dev {dev} unreachable: {e}");
            battery_idx.remove(&dev);
            state.set_online(dev, false);
        }
    }
    Ok(())
}

/// Query a device's name, kind and battery; unreachable devices are marked offline.
fn probe(rcv: &mut Receiver, state: &mut State, battery_idx: &mut HashMap<u8, u8>, dev: u8) -> io::Result<()> {
    match rcv.probe_device(dev) {
        Ok(found) => {
            debug!("dev {dev}: {:?}", found.device);
            if let Some(idx) = found.battery_idx {
                battery_idx.insert(dev, idx);
            }
            notify(state.upsert(found.device));
        }
        Err(ReqError::Io(e)) => return Err(e),
        Err(e) => {
            debug!("dev {dev} unreachable: {e}");
            state.set_online(dev, false);
        }
    }
    Ok(())
}

fn notify(alerts: Vec<Alert>) {
    for alert in alerts {
        info!("low battery: {} {}%", alert.name, alert.percent);
        let shown = notify_rust::Notification::new()
            .appname(tray::TITLE)
            .summary(&format!("{} battery low", alert.name))
            .body(&format!("{}% remaining", alert.percent))
            .icon("battery-caution")
            .show();
        if let Err(e) = shown {
            warn!("cannot show notification: {e}");
        }
    }
}
