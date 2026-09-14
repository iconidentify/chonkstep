//! System-bus resume bridge.
//!
//! A system suspend does not pause the libseat session, so the VT-resume
//! path in `session.rs` never runs on the way back from sleep — and that
//! path is the only one that re-examines the connectors and the crtcs.
//! logind announces both edges of a suspend with
//! `org.freedesktop.login1.Manager.PrepareForSleep(bool)`. This module
//! owns one small blocking zbus thread on the system bus, shaped like
//! `inhibit_bus.rs`, that watches for the `false` (resumed) edge and
//! sends it into calloop. The compositor side,
//! [`crate::session::note_system_resumed`], arms a connector rescan
//! (which reads every link's status), reprograms gamma and forces a full
//! repaint. No D-Bus call ever runs on the compositor thread, and the
//! thread carries no policy: the only thing that crosses is the edge.
//!
//! A missing system bus, or a system without logind, is not an error
//! worth more than a log line: the failure shapes this covers still have
//! the per-output escalation and the udev link check behind them.

use calloop::channel::{self, Event, SyncSender};
use calloop::LoopHandle;

use crate::state::Compositor;

const LOGIN1_NAME: &str = "org.freedesktop.login1";
const LOGIN1_PATH: &str = "/org/freedesktop/login1";
const LOGIN1_MANAGER: &str = "org.freedesktop.login1.Manager";
const PREPARE_FOR_SLEEP: &str = "PrepareForSleep";

#[derive(Debug, PartialEq, Eq)]
enum BusEvent {
    Resumed,
}

/// Registers the compositor-side channel, then starts the watcher
/// thread. Called from the session backend only: the nested backend
/// runs inside someone else's session and has no crtcs to look after.
pub(crate) fn init(loop_handle: &LoopHandle<'static, Compositor>) {
    // One queued wake is enough: the edge is idempotent, and two resumes
    // that land before the loop runs are one resume as far as the
    // display is concerned.
    let (sender, receiver) = channel::sync_channel(1);
    if let Err(error) = loop_handle.insert_source(receiver, |event, _, comp| match event {
        Event::Msg(BusEvent::Resumed) => crate::session::note_system_resumed(comp),
        Event::Closed => tracing::debug!("system-bus sleep bridge stopped"),
    }) {
        tracing::warn!(?error, "could not register system-bus resume events");
        return;
    }

    if let Err(error) = std::thread::Builder::new()
        .name("chonkstep-sleep-bus".into())
        .spawn(move || {
            if let Err(error) = watch(sender) {
                tracing::warn!(?error, "system-bus resume notifications unavailable");
            }
        })
    {
        tracing::warn!(?error, "could not start the system-bus sleep watcher");
    }
}

fn watch(sender: SyncSender<BusEvent>) -> zbus::Result<()> {
    let connection = zbus::blocking::Connection::system()?;
    let proxy = zbus::blocking::Proxy::new(&connection, LOGIN1_NAME, LOGIN1_PATH, LOGIN1_MANAGER)?;
    let signals = proxy.receive_signal(PREPARE_FOR_SLEEP)?;
    tracing::info!(signal = PREPARE_FOR_SLEEP, "watching logind for resume from sleep");
    for message in signals {
        if !deliver(&sender, message.body().deserialize::<bool>()) {
            break;
        }
    }
    Ok(())
}

/// Forwards one `PrepareForSleep` body: only the `false` edge — the
/// system is awake again — reaches the compositor. Returns `false` once
/// the compositor side is gone and the thread should stop.
fn deliver(sender: &SyncSender<BusEvent>, body: zbus::Result<bool>) -> bool {
    match body {
        Ok(true) => tracing::debug!("system is preparing to sleep"),
        Ok(false) => match sender.try_send(BusEvent::Resumed) {
            Ok(()) => tracing::debug!("system resumed; edge queued for the compositor"),
            // Full means an equivalent edge is already queued.
            Err(std::sync::mpsc::TrySendError::Full(_)) => {}
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => return false,
        },
        Err(error) => tracing::warn!(?error, "ignored malformed PrepareForSleep signal"),
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sleep edge stays on the bus thread; the wake edge crosses,
    /// and a second wake before the loop drains the first coalesces
    /// into it rather than queueing a second rescan.
    #[test]
    fn only_the_wake_edge_crosses_and_coalesces() {
        let (sender, receiver) = channel::sync_channel(1);
        assert!(deliver(&sender, Ok(true)));
        assert!(matches!(receiver.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));

        assert!(deliver(&sender, Ok(false)));
        assert!(deliver(&sender, Ok(false)));
        assert_eq!(receiver.try_recv().unwrap(), BusEvent::Resumed);
        assert!(matches!(receiver.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));

        assert!(deliver(&sender, Err(zbus::Error::MissingParameter("body"))));
        assert!(matches!(receiver.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));

        drop(receiver);
        assert!(!deliver(&sender, Ok(false)), "a closed compositor side stops the thread");
    }
}
