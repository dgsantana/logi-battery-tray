mod hidpp;
mod state;
mod transport;
mod tray;

use std::collections::HashMap;
use std::io;
use std::process::ExitCode;
use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};

use hidapi::HidApi;
use ksni::blocking::TrayMethods;
use log::{debug, error, info, warn};

use crate::hidpp::Message;
use crate::transport::{ReqError, Transport};
use crate::state::{Alert, State};
use crate::tray::{BatteryTray, Cmd};

const POLL_INTERVAL: Duration = Duration::from_secs(5 * 60);
const REDISCOVER_INTERVAL: Duration = Duration::from_secs(10);
const EVENT_WAIT: Duration = Duration::from_secs(1);
/// Retry delay when a device that just linked up does not answer yet.
const LINK_RETRY: Duration = Duration::from_secs(5);

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
    let Some((opened, id)) = open_first() else {
        eprintln!("No Logitech devices found");
        return ExitCode::FAILURE;
    };
    let mut rcv = match opened {
        Ok(rcv) => rcv,
        Err(e) => {
            eprintln!("cannot open {id}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut state = State::default();
    let mut battery_idx = HashMap::new();
    for &dev in rcv.indexes() {
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
        let mut rcv = match open_first() {
            Some((Ok(rcv), id)) => {
                info!("using transport {id}");
                state.set_present(&id, true);
                rcv
            }
            Some((Err(e), id)) => {
                warn!("cannot open {id}: {e}");
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
                state.set_present(rcv.id(), false);
                publish(&state, state.any_present());
            }
        }
    }
}

/// Open the first transport found, with its id.
fn open_first() -> Option<(io::Result<Transport>, String)> {
    let api = match HidApi::new() {
        Ok(api) => api,
        Err(e) => {
            warn!("cannot enumerate HID devices: {e}");
            return None;
        }
    };
    let candidate = transport::discover(&api).into_iter().next()?;
    Some((Transport::open(&api, &candidate), candidate.id))
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
    rcv: &mut Transport,
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
            for &dev in rcv.indexes() {
                refresh(rcv, state, &mut battery_idx, dev)?;
            }
            publish(state, true);
            next_poll = Instant::now() + POLL_INTERVAL;
        }
        match rcv.next_event(EVENT_WAIT)? {
            Some(Message::Event { dev, feat_idx, func: 0, data }) if battery_idx.get(&dev) == Some(&feat_idx) => {
                let Some(battery) = hidpp::parse_battery(&data) else { continue };
                debug!("battery event dev {dev}: {battery:?}");
                notify(state.set_battery(&rcv.key(dev), battery));
                publish(state, true);
            }
            Some(Message::Connection { dev, linked }) => {
                debug!("dev {dev} link {}", if linked { "up" } else { "down" });
                battery_idx.remove(&dev);
                if linked {
                    let reachable = probe(rcv, state, &mut battery_idx, dev)?;
                    next_poll = poll_after_link_up(reachable, Instant::now(), next_poll);
                } else {
                    state.set_online(&rcv.key(dev), false);
                }
                publish(state, true);
            }
            _ => {}
        }
    }
}

/// Re-read a known device's battery, or probe it from scratch.
fn refresh(rcv: &mut Transport, state: &mut State, battery_idx: &mut HashMap<u8, u8>, dev: u8) -> io::Result<()> {
    let Some(&idx) = battery_idx.get(&dev) else {
        return probe(rcv, state, battery_idx, dev).map(drop);
    };
    match rcv.read_battery(dev, idx) {
        Ok(battery) => notify(state.set_battery(&rcv.key(dev), battery)),
        Err(ReqError::Io(e)) => return Err(e),
        Err(e) => {
            debug!("dev {dev} unreachable: {e}");
            battery_idx.remove(&dev);
            state.set_online(&rcv.key(dev), false);
        }
    }
    Ok(())
}

/// Query a device's name, kind and battery; unreachable devices are marked offline.
/// Returns whether the device answered.
fn probe(rcv: &mut Transport, state: &mut State, battery_idx: &mut HashMap<u8, u8>, dev: u8) -> io::Result<bool> {
    match rcv.probe_device(dev) {
        Ok(found) => {
            debug!("dev {dev}: {:?}", found.device);
            if let Some(idx) = found.battery_idx {
                battery_idx.insert(dev, idx);
            }
            notify(state.upsert(found.device));
            Ok(true)
        }
        Err(ReqError::Io(e)) => Err(e),
        Err(e) => {
            debug!("dev {dev} unreachable: {e}");
            state.set_online(&rcv.key(dev), false);
            Ok(false)
        }
    }
}

/// When the next poll is due after a link-up probe; pulled forward if the device did not answer.
fn poll_after_link_up(reachable: bool, now: Instant, next_poll: Instant) -> Instant {
    if reachable { next_poll } else { next_poll.min(now + LINK_RETRY) }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreachable_link_up_pulls_poll_forward() {
        let now = Instant::now();
        let later = now + POLL_INTERVAL;
        assert_eq!(poll_after_link_up(false, now, later), now + LINK_RETRY);
    }

    #[test]
    fn reachable_link_up_keeps_schedule() {
        let now = Instant::now();
        let later = now + POLL_INTERVAL;
        assert_eq!(poll_after_link_up(true, now, later), later);
    }

    #[test]
    fn retry_never_delays_an_earlier_poll() {
        let now = Instant::now();
        let soon = now + Duration::from_secs(1);
        assert_eq!(poll_after_link_up(false, now, soon), soon);
    }
}
