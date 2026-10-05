//! Worker loop: discover transports, track their devices, publish snapshots.

use std::collections::HashMap;
use std::io;
use std::process::ExitCode;
use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};

use hidapi::HidApi;
use log::{debug, info, warn};

use crate::hidpp::{self, BatteryStatus, Message};
use crate::state::{Alert, State};
use crate::transport::{self, Candidate, ReqError, Transport};
use crate::tray::{self, Cmd, Snapshot};

const POLL_INTERVAL: Duration = Duration::from_secs(5 * 60);
const REDISCOVER_INTERVAL: Duration = Duration::from_secs(10);
/// How long one pass waits for events, shared across open transports.
const EVENT_WAIT: Duration = Duration::from_secs(1);
/// Retry delay when a device that just linked up does not answer yet.
const LINK_RETRY: Duration = Duration::from_secs(5);

/// One open transport and what we know about its devices.
struct Link {
    /// What was opened, to notice when its collections change.
    candidate: Candidate,
    transport: Transport,
    /// Feature index of UNIFIED_BATTERY per online device.
    battery_idx: HashMap<u8, u8>,
    next_poll: Instant,
}

/// Candidates to open, and open ids to drop. A transport whose collections
/// changed (e.g. Windows still adding them on hot-plug) is dropped and reopened.
pub fn reconcile(open: &[Candidate], found: &[Candidate]) -> (Vec<Candidate>, Vec<String>) {
    let to_open = found.iter().filter(|c| !open.contains(c)).cloned().collect();
    let to_drop = open.iter().filter(|c| !found.contains(c)).map(|c| c.id.clone()).collect();
    (to_open, to_drop)
}

fn enumerate() -> Option<(HidApi, Vec<Candidate>)> {
    match HidApi::new() {
        Ok(api) => {
            let found = transport::discover(&api);
            Some((api, found))
        }
        Err(e) => {
            warn!("cannot enumerate HID devices: {e}");
            None
        }
    }
}

/// Print every device's battery once and exit.
pub fn run_once() -> ExitCode {
    let found = enumerate().filter(|(_, found)| !found.is_empty());
    let Some((api, found)) = found else {
        eprintln!("No Logitech devices found");
        return ExitCode::FAILURE;
    };
    let mut state = State::default();
    for candidate in &found {
        let mut link = match Transport::open(&api, candidate) {
            Ok(t) => {
                state.set_present(&candidate.id, true);
                Link { candidate: candidate.clone(), transport: t, battery_idx: HashMap::new(), next_poll: Instant::now() }
            }
            Err(e) => {
                eprintln!("cannot open {}: {e}", candidate.id);
                continue;
            }
        };
        let mut alerts = Vec::new();
        for &dev in link.transport.indexes() {
            if let Err(e) = probe(&mut link, &mut state, &mut alerts, dev) {
                eprintln!("I/O error on {}: {e}", candidate.id);
                break;
            }
        }
    }
    let snap = Snapshot::of(&state);
    if !snap.present {
        return ExitCode::FAILURE;
    }
    println!("{}", tray::summary(&snap.devices, true));
    ExitCode::SUCCESS
}

/// Track every transport until Quit, publishing a snapshot after each change.
pub fn run(cmds: &mpsc::Receiver<Cmd>, publish: impl Fn(Snapshot), notify: impl Fn(&Alert)) {
    let mut state = State::default();
    let mut links: Vec<Link> = Vec::new();
    let mut next_discover = Instant::now();
    loop {
        match cmds.try_recv() {
            Ok(Cmd::Quit) | Err(TryRecvError::Disconnected) => return,
            Ok(Cmd::Refresh) => links.iter_mut().for_each(|l| l.next_poll = Instant::now()),
            Err(TryRecvError::Empty) => {}
        }
        let mut alerts = Vec::new();
        let mut changed = false;
        if Instant::now() >= next_discover {
            changed |= rediscover(&mut links, &mut state);
            next_discover = Instant::now() + REDISCOVER_INTERVAL;
        }
        let mut dead = Vec::new();
        for (i, link) in links.iter_mut().enumerate() {
            if Instant::now() < link.next_poll {
                continue;
            }
            let polled = link
                .transport
                .indexes()
                .iter()
                .try_for_each(|&dev| refresh(link, &mut state, &mut alerts, dev));
            match polled {
                Ok(()) => link.next_poll = Instant::now() + POLL_INTERVAL,
                Err(e) => {
                    warn!("lost {}: {e}", link.transport.id());
                    dead.push(i);
                }
            }
            changed = true;
        }
        if links.is_empty() {
            if changed {
                publish(Snapshot::of(&state));
            }
            match cmds.recv_timeout(next_discover.saturating_duration_since(Instant::now())) {
                Ok(Cmd::Quit) | Err(RecvTimeoutError::Disconnected) => return,
                Ok(Cmd::Refresh) => next_discover = Instant::now(),
                Err(RecvTimeoutError::Timeout) => {}
            }
            continue;
        }
        let wait = EVENT_WAIT / links.len() as u32;
        for (i, link) in links.iter_mut().enumerate() {
            if dead.contains(&i) {
                continue;
            }
            match handle_event(link, &mut state, &mut alerts, wait) {
                Ok(c) => changed |= c,
                Err(e) => {
                    warn!("lost {}: {e}", link.transport.id());
                    dead.push(i);
                    changed = true;
                }
            }
        }
        dead.sort_unstable();
        for i in dead.into_iter().rev() {
            let link = links.remove(i);
            state.set_present(link.transport.id(), false);
        }
        alerts.iter().for_each(&notify);
        if changed {
            publish(Snapshot::of(&state));
        }
    }
}

/// Open new transports and drop vanished ones. Returns whether anything changed.
fn rediscover(links: &mut Vec<Link>, state: &mut State) -> bool {
    let Some((api, found)) = enumerate() else { return false };
    let open: Vec<Candidate> = links.iter().map(|l| l.candidate.clone()).collect();
    let (to_open, to_drop) = reconcile(&open, &found);
    let mut changed = !to_drop.is_empty();
    links.retain(|l| !to_drop.iter().any(|id| id == l.transport.id()));
    for id in &to_drop {
        info!("{id} went away");
        state.set_present(id, false);
    }
    for candidate in to_open {
        match Transport::open(&api, &candidate) {
            Ok(transport) => {
                info!("using {:?} transport {}", candidate.kind, candidate.id);
                state.set_present(&candidate.id, true);
                links.push(Link { candidate, transport, battery_idx: HashMap::new(), next_poll: Instant::now() });
                changed = true;
            }
            Err(e) => debug!("cannot open {}: {e}", candidate.id),
        }
    }
    changed
}

/// What an incoming message asks the worker to do.
#[derive(Debug, PartialEq, Eq)]
enum EventAction {
    Battery { dev: u8, battery: BatteryStatus },
    LinkUp(u8),
    LinkDown(u8),
    /// A device we have no battery feature for spoke: probe it. BLE links send
    /// no connection notifications, so this is how a waking device is noticed.
    Wake(u8),
    Ignore,
}

fn classify_event(msg: Option<Message>, battery_idx: &HashMap<u8, u8>) -> EventAction {
    match msg {
        Some(Message::Event { dev, feat_idx, func, data }) => match battery_idx.get(&dev) {
            None => EventAction::Wake(dev),
            Some(&idx) if idx == feat_idx && func == 0 => {
                hidpp::parse_battery(&data).map_or(EventAction::Ignore, |battery| EventAction::Battery { dev, battery })
            }
            Some(_) => EventAction::Ignore,
        },
        Some(Message::Connection { dev, linked: true }) => EventAction::LinkUp(dev),
        Some(Message::Connection { dev, linked: false }) => EventAction::LinkDown(dev),
        _ => EventAction::Ignore,
    }
}

/// Wait for one event on a link and apply it. Returns whether state changed.
fn handle_event(link: &mut Link, state: &mut State, alerts: &mut Vec<Alert>, wait: Duration) -> io::Result<bool> {
    match classify_event(link.transport.next_event(wait)?, &link.battery_idx) {
        EventAction::Battery { dev, battery } => {
            debug!("battery event dev {dev}: {battery:?}");
            alerts.extend(state.set_battery(&link.transport.key(dev), battery));
        }
        EventAction::LinkUp(dev) => {
            debug!("dev {dev} link up");
            link.battery_idx.remove(&dev);
            let reachable = probe(link, state, alerts, dev)?;
            link.next_poll = poll_after_link_up(reachable, Instant::now(), link.next_poll);
        }
        EventAction::LinkDown(dev) => {
            debug!("dev {dev} link down");
            link.battery_idx.remove(&dev);
            state.set_online(&link.transport.key(dev), false);
        }
        EventAction::Wake(dev) => {
            debug!("event from unprobed dev {dev}, probing");
            probe(link, state, alerts, dev)?;
        }
        EventAction::Ignore => return Ok(false),
    }
    Ok(true)
}

/// Re-read a known device's battery, or probe it from scratch.
fn refresh(link: &mut Link, state: &mut State, alerts: &mut Vec<Alert>, dev: u8) -> io::Result<()> {
    let Some(&idx) = link.battery_idx.get(&dev) else {
        return probe(link, state, alerts, dev).map(drop);
    };
    match link.transport.read_battery(dev, idx) {
        Ok(battery) => alerts.extend(state.set_battery(&link.transport.key(dev), battery)),
        Err(ReqError::Io(e)) => return Err(e),
        Err(e) => {
            debug!("dev {dev} unreachable: {e}");
            link.battery_idx.remove(&dev);
            state.set_online(&link.transport.key(dev), false);
        }
    }
    Ok(())
}

/// Query a device's name, kind and battery; unreachable devices are marked offline.
/// Returns whether the device answered.
fn probe(link: &mut Link, state: &mut State, alerts: &mut Vec<Alert>, dev: u8) -> io::Result<bool> {
    match link.transport.probe_device(dev) {
        Ok(found) => {
            debug!("dev {dev}: {:?}", found.device);
            if let Some(idx) = found.battery_idx {
                link.battery_idx.insert(dev, idx);
            }
            alerts.extend(state.upsert(found.device));
            Ok(true)
        }
        Err(ReqError::Io(e)) => Err(e),
        Err(e) => {
            debug!("dev {dev} unreachable: {e}");
            state.set_online(&link.transport.key(dev), false);
            Ok(false)
        }
    }
}

/// When the next poll is due after a link-up probe; pulled forward if the device did not answer.
fn poll_after_link_up(reachable: bool, now: Instant, next_poll: Instant) -> Instant {
    if reachable { next_poll } else { next_poll.min(now + LINK_RETRY) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{HidPath, Kind};

    fn cand(id: &str) -> Candidate {
        Candidate { id: id.into(), kind: Kind::Receiver, paths: Vec::new() }
    }

    fn cand_paths(id: &str, paths: &[&str]) -> Candidate {
        let paths = paths.iter().map(|p| HidPath { path: std::ffi::CString::new(*p).unwrap(), usage: 1 }).collect();
        Candidate { id: id.into(), kind: Kind::Receiver, paths }
    }

    #[test]
    fn reconcile_opens_new_and_drops_vanished() {
        let open = [cand("kept"), cand("gone")];
        let found = [cand("kept"), cand("new")];
        let (to_open, to_drop) = reconcile(&open, &found);
        assert_eq!(to_open, [cand("new")]);
        assert_eq!(to_drop, ["gone"]);
    }

    #[test]
    fn reconcile_with_nothing_changed_is_a_no_op() {
        let (to_open, to_drop) = reconcile(&[cand("a")], &[cand("a")]);
        assert!(to_open.is_empty());
        assert!(to_drop.is_empty());
    }
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

    #[test]
    fn reconcile_reopens_transport_whose_paths_changed() {
        let open = [cand_paths("r", &["col01"])];
        let found = [cand_paths("r", &["col01", "col02"])];
        let (to_open, to_drop) = reconcile(&open, &found);
        assert_eq!(to_drop, ["r"]);
        assert_eq!(to_open, [found[0].clone()]);
    }

    fn known(dev: u8, idx: u8) -> HashMap<u8, u8> {
        HashMap::from([(dev, idx)])
    }

    fn event(dev: u8, feat_idx: u8, func: u8) -> Option<Message> {
        Some(Message::Event { dev, feat_idx, func, data: vec![50, 4, 0, 0] })
    }

    #[test]
    fn battery_event_from_known_device_updates_battery() {
        assert!(matches!(classify_event(event(2, 6, 0), &known(2, 6)), EventAction::Battery { dev: 2, .. }));
    }

    #[test]
    fn any_event_from_unknown_device_wakes_it() {
        assert_eq!(classify_event(event(0xFF, 3, 0), &HashMap::new()), EventAction::Wake(0xFF));
    }

    #[test]
    fn other_events_from_known_device_are_ignored() {
        assert_eq!(classify_event(event(2, 9, 0), &known(2, 6)), EventAction::Ignore);
        assert_eq!(classify_event(event(2, 6, 1), &known(2, 6)), EventAction::Ignore);
        assert_eq!(classify_event(None, &known(2, 6)), EventAction::Ignore);
    }

    #[test]
    fn connection_notifications_map_to_link_changes() {
        let up = Some(Message::Connection { dev: 3, linked: true });
        let down = Some(Message::Connection { dev: 3, linked: false });
        assert_eq!(classify_event(up, &HashMap::new()), EventAction::LinkUp(3));
        assert_eq!(classify_event(down, &known(3, 6)), EventAction::LinkDown(3));
    }
}
