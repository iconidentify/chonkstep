//! Clamshell lid bridge.
//!
//! Whether closing the lid suspends is decided by systemd-logind:
//! `HandleLidSwitchDocked` (default `ignore`) applies only while logind
//! counts an external display, and it counts one only when the DRM
//! connector's type is on a fixed list (`DP-`, `HDMI-A-`, `DVI-`, ...).
//! Apple silicon names its USB-C outputs `USB-1`/`USB-2`, which is not on
//! that list, so a laptop driving a Type-C or Thunderbolt display is
//! never "docked" and a lid close always suspends it.
//!
//! The compositor knows which outputs it drives, so it owns the policy,
//! as the macOS window server and GNOME do: while at least one external
//! output is driven, hold a logind `handle-lid-switch` block inhibitor.
//! logind then leaves the lid to the session, whose lid bindings park the
//! internal panel. When the last external output goes, the inhibitor is
//! dropped and a lid close suspends as usual.
//!
//! Like `sleep_bus.rs`, the D-Bus side is one small blocking thread. The
//! compositor thread only sends the desired state, and only when it
//! changes, over a non-blocking channel; the thread takes or releases the
//! inhibitor. The inhibitor is a file descriptor: dropping it (or the
//! compositor exiting) releases it.

use std::sync::mpsc::{self, Receiver, Sender};

const LOGIN1_NAME: &str = "org.freedesktop.login1";
const LOGIN1_PATH: &str = "/org/freedesktop/login1";
const LOGIN1_MANAGER: &str = "org.freedesktop.login1.Manager";

/// Connector types that are part of the machine itself. Everything else
/// counts as external: the same deny list Omarchy's
/// `omarchy-hw-external-monitors` uses, so both agree on "docked".
const INTERNAL_PREFIXES: [&str; 3] = ["eDP-", "LVDS-", "DSI-"];

/// Whether `name` (`eDP-1`, `USB-2`, `HDMI-A-1`) is a built-in panel.
pub(crate) fn is_internal_connector(name: &str) -> bool {
    INTERNAL_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

/// Whether the driven outputs call for keeping the machine awake with the
/// lid closed: true as soon as one of them is external.
pub(crate) fn wants_inhibit<'a>(driven: impl IntoIterator<Item = &'a str>) -> bool {
    driven.into_iter().any(|name| !is_internal_connector(name))
}

/// Compositor-side handle. Sends the desired inhibitor state to the bus
/// thread on each change and never blocks.
pub(crate) struct LidInhibitor {
    sender: Option<Sender<bool>>,
    /// Last state sent, so steady-state rescans send nothing.
    sent: Option<bool>,
}

impl LidInhibitor {
    /// Starts the bus thread. If it cannot be started, the handle stays
    /// inert and lid handling falls back to plain logind policy.
    pub(crate) fn start() -> Self {
        let (sender, receiver) = mpsc::channel();
        match std::thread::Builder::new()
            .name("chonkstep-lid-bus".into())
            .spawn(move || run(receiver))
        {
            Ok(_) => Self::with_sender(Some(sender)),
            Err(error) => {
                tracing::warn!(?error, "could not start the clamshell lid bridge");
                Self::with_sender(None)
            }
        }
    }

    fn with_sender(sender: Option<Sender<bool>>) -> Self {
        Self { sender, sent: None }
    }

    /// Re-evaluates the policy for the outputs now being driven.
    pub(crate) fn update<'a>(&mut self, driven: impl IntoIterator<Item = &'a str>) {
        let want = wants_inhibit(driven);
        if self.sent == Some(want) {
            return;
        }
        let Some(sender) = &self.sender else {
            return;
        };
        if sender.send(want).is_err() {
            // The bus thread is gone; stop trying.
            self.sender = None;
            return;
        }
        self.sent = Some(want);
        if want {
            tracing::info!("external output driven: keeping the machine awake with the lid closed");
        } else {
            tracing::info!("no external output: a lid close suspends as configured");
        }
    }
}

fn run(receiver: Receiver<bool>) {
    let mut connection: Option<zbus::blocking::Connection> = None;
    let mut held: Option<zbus::zvariant::OwnedFd> = None;
    while let Ok(mut want) = receiver.recv() {
        // Only the latest state matters.
        while let Ok(newer) = receiver.try_recv() {
            want = newer;
        }
        reconcile(want, &mut held, || acquire(&mut connection));
    }
    // The compositor side is gone; `held` drops here and logind releases
    // the inhibitor.
}

/// Brings `held` in line with `want`: takes the inhibitor on the first
/// `true`, drops it on `false`, and retries a failed take on the next
/// `true`.
fn reconcile<T, E: std::fmt::Display>(want: bool, held: &mut Option<T>, acquire: impl FnOnce() -> Result<T, E>) {
    match (want, held.is_some()) {
        (true, false) => match acquire() {
            Ok(inhibitor) => {
                *held = Some(inhibitor);
                tracing::info!("holding logind handle-lid-switch inhibitor (clamshell mode)");
            }
            Err(error) => tracing::warn!(%error, "could not take the logind lid-switch inhibitor"),
        },
        (false, true) => {
            *held = None;
            tracing::info!("released logind handle-lid-switch inhibitor");
        }
        _ => {}
    }
}

fn acquire(connection: &mut Option<zbus::blocking::Connection>) -> zbus::Result<zbus::zvariant::OwnedFd> {
    if connection.is_none() {
        *connection = Some(zbus::blocking::Connection::system()?);
    }
    let Some(connection) = connection.as_ref() else {
        return Err(zbus::Error::Failure("no system bus connection".into()));
    };
    let proxy = zbus::blocking::Proxy::new(connection, LOGIN1_NAME, LOGIN1_PATH, LOGIN1_MANAGER)?;
    proxy.call(
        "Inhibit",
        &(
            "handle-lid-switch",
            "chonkstep",
            "An external display is in use (clamshell mode)",
            "block",
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The case this module exists for: logind does not count `USB-*`
    /// connectors, so a Type-C display must count as external here.
    #[test]
    fn a_type_c_output_is_external_and_panels_are_not() {
        assert!(wants_inhibit(["eDP-1", "USB-1"]));
        assert!(wants_inhibit(["HDMI-A-1"]));
        assert!(wants_inhibit(["DP-2"]));
        assert!(!wants_inhibit(["eDP-1"]));
        assert!(!wants_inhibit(["eDP-1", "DSI-1"]));
        assert!(!wants_inhibit(["LVDS-1"]));
        assert!(!wants_inhibit(std::iter::empty::<&str>()));
    }

    /// Only edges cross the channel: a steady rescan sends nothing.
    #[test]
    fn only_state_changes_are_sent() {
        let (sender, receiver) = mpsc::channel();
        let mut lid = LidInhibitor::with_sender(Some(sender));

        lid.update(["eDP-1"]);
        lid.update(["eDP-1"]);
        lid.update(["eDP-1", "USB-1"]);
        lid.update(["USB-1"]);
        lid.update(["eDP-1"]);

        let sent: Vec<bool> = receiver.try_iter().collect();
        assert_eq!(sent, [false, true, false]);
    }

    /// A vanished bus thread does not make the compositor side fail.
    #[test]
    fn a_closed_bus_thread_is_tolerated() {
        let (sender, receiver) = mpsc::channel();
        drop(receiver);
        let mut lid = LidInhibitor::with_sender(Some(sender));
        lid.update(["USB-1"]);
        lid.update(["eDP-1"]);
        assert!(lid.sender.is_none());
    }

    /// Take on the first `true`, keep while `true`, drop on `false`, and
    /// retry a failed take the next time it is wanted.
    #[test]
    fn reconcile_takes_holds_and_releases() {
        let mut held: Option<u32> = None;
        let mut takes = 0;

        reconcile(true, &mut held, || -> Result<u32, String> { Err("denied".into()) });
        assert_eq!(held, None);

        reconcile(true, &mut held, || -> Result<u32, String> {
            takes += 1;
            Ok(7)
        });
        assert_eq!(held, Some(7));

        reconcile(true, &mut held, || -> Result<u32, String> {
            takes += 1;
            Ok(8)
        });
        assert_eq!(held, Some(7), "an already held inhibitor is kept");

        reconcile(false, &mut held, || -> Result<u32, String> { unreachable!() });
        assert_eq!(held, None);
        assert_eq!(takes, 1);
    }
}
