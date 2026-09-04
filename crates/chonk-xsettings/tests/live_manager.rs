//! The ICCCM choreography, against a real X server.
//!
//! Every test here is `#[ignore]`d, so `cargo test` on a build machine
//! with no display runs none of them and the crate's guarantee — that it
//! is fully testable without an X server — is untouched. They exist
//! because the other half of the crate is *not* testable that way, and
//! "the byte format is proven and the selection dance is hoped for" is
//! not a state to leave a manager in: the failure modes in
//! `manager.rs` (property written after ownership, a missing read-back,
//! a `CurrentTime` where a timestamp belongs) are all invisible to a
//! pure test and all obvious to a real server.
//!
//! Run them with a display present:
//!
//! ```text
//! Xvfb :99 -screen 0 1280x800x24 &
//! DISPLAY=:99 cargo test -p chonk-xsettings -- --ignored --test-threads=1
//! ```
//!
//! `gtk-query-settings` (from `libgtk-3-bin`) is also required for the
//! consumer-side scale test. CI installs it explicitly before running
//! this file, so a missing probe is a test failure rather than a skip.
//!
//! `--test-threads=1` is not optional. Every test in this file competes
//! for the *same* `_XSETTINGS_S0` selection on the same display, which
//! is precisely the resource the crate exists to hold exclusively;
//! running them concurrently would have them fail each other on purpose.

use std::process::Command;
use std::time::{Duration, Instant};

use x11rb::COPY_DEPTH_FROM_PARENT;
use x11rb::connection::Connection as _;
use x11rb::errors::ConnectError;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt as _, CreateWindowAux, PropMode, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use chonk_xsettings::{
    AcquisitionPolicy, DesktopAppearance, ManagerState, Settings, XSettingsError, XSettingsManager,
    keys,
};

const X_SERVER_SETTLE: Duration = Duration::from_secs(1);

/// Xvfb can reset a just-accepted connection while it is still
/// finishing an earlier client's teardown. That is transport churn,
/// not a failed XSETTINGS assertion, so retry only those transient
/// handshake errors within a small, explicit budget.
fn connect_to_display(role: &str) -> (RustConnection, usize) {
    let deadline = Instant::now() + X_SERVER_SETTLE;
    loop {
        match RustConnection::connect(None) {
            Ok(connection) => return connection,
            Err(ConnectError::IoError(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::Interrupted
                ) && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("{role}: {error:?}"),
        }
    }
}

fn acquire_manager(policy: AcquisitionPolicy) -> Result<XSettingsManager, XSettingsError> {
    let (connection, screen) = connect_to_display("a display for the XSETTINGS manager");
    XSettingsManager::acquire_with_connection_and_policy(connection, screen, policy)
}

/// An observer connection, standing in for an X client that wants to
/// know what the settings manager is publishing. Separate from the
/// manager's own connection on purpose: everything asserted below is
/// asserted the way a *client* would see it, not by reading the
/// manager's own memory.
struct Observer {
    conn: RustConnection,
    selection: u32,
    settings_atom: u32,
}

impl Observer {
    fn new() -> Self {
        let (conn, screen) = connect_to_display("a display to observe");
        let selection = conn
            .intern_atom(false, format!("_XSETTINGS_S{screen}").as_bytes())
            .unwrap()
            .reply()
            .unwrap()
            .atom;
        let settings_atom = conn
            .intern_atom(false, b"_XSETTINGS_SETTINGS")
            .unwrap()
            .reply()
            .unwrap()
            .atom;
        Self {
            conn,
            selection,
            settings_atom,
        }
    }

    fn owner(&self) -> u32 {
        self.conn
            .get_selection_owner(self.selection)
            .unwrap()
            .reply()
            .unwrap()
            .owner
    }

    fn await_owner(&self, expected: u32, reason: &str) {
        let deadline = Instant::now() + X_SERVER_SETTLE;
        loop {
            let actual = self.owner();
            if actual == expected {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{reason}: expected owner {expected}, got {actual}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The raw `_XSETTINGS_SETTINGS` property, as a client reads it:
    /// type and format checked, because a client that finds the wrong
    /// ones ignores the property entirely.
    fn property(&self, window: u32) -> Option<Vec<u8>> {
        let reply = self
            .conn
            .get_property(false, window, self.settings_atom, AtomEnum::ANY, 0, u32::MAX / 4)
            .unwrap()
            .reply()
            .unwrap();
        if reply.type_ == 0 {
            return None;
        }
        assert_eq!(
            reply.type_, self.settings_atom,
            "the property type must be _XSETTINGS_SETTINGS"
        );
        assert_eq!(reply.format, 8, "the property must be at format 8");
        Some(reply.value)
    }
}

/// A pretend incumbent: a connection that owns `_XSETTINGS_S0` the way
/// XWayland does, publishing whatever bytes the test hands it. Built
/// with raw `x11rb` rather than through `XSettingsManager` on purpose —
/// the thing being played here is precisely a selection owner that is
/// *not* this crate, and the placeholder case must reproduce XWayland's
/// stub exactly: an owner window, an empty (or not) settings block, and
/// no intention of ever standing down by itself.
struct FakeOwner {
    conn: RustConnection,
    window: u32,
}

impl FakeOwner {
    /// Takes the selection and publishes `property` on the owner
    /// window; `None` owns the selection with no property at all.
    fn claiming(property: Option<&[u8]>) -> Self {
        let (conn, screen_num) = connect_to_display("a display to squat on");
        let screen = &conn.setup().roots[screen_num];
        let selection = conn
            .intern_atom(false, format!("_XSETTINGS_S{screen_num}").as_bytes())
            .unwrap()
            .reply()
            .unwrap()
            .atom;
        let settings_atom = conn
            .intern_atom(false, b"_XSETTINGS_SETTINGS")
            .unwrap()
            .reply()
            .unwrap()
            .atom;

        let window = conn.generate_id().unwrap();
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            screen.root,
            -1,
            -1,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new().override_redirect(1),
        )
        .unwrap()
        .check()
        .unwrap();
        if let Some(bytes) = property {
            conn.change_property8(PropMode::REPLACE, window, settings_atom, settings_atom, bytes)
                .unwrap()
                .check()
                .unwrap();
        }
        // `CurrentTime` would be wrong in the real manager; for a test
        // stub it matches what XWayland effectively does and keeps the
        // helper simple.
        conn.set_selection_owner(window, selection, 0u32)
            .unwrap()
            .check()
            .unwrap();
        conn.flush().unwrap();
        assert_eq!(
            conn.get_selection_owner(selection).unwrap().reply().unwrap().owner,
            window,
            "the fake owner must actually own the selection before the test means anything"
        );
        // The server stamped the selection with its clock just now; a
        // takeover's own timestamp must be strictly later, so give the
        // clock a tick to move before the contender fetches one.
        std::thread::sleep(std::time::Duration::from_millis(20));
        Self { conn, window }
    }

    /// Blocks until the server tells this owner it lost the selection.
    fn lost_the_selection(&self) -> bool {
        loop {
            match self.conn.wait_for_event() {
                Ok(Event::SelectionClear(event)) => return event.owner == self.window,
                Ok(_) => continue,
                Err(_) => return false,
            }
        }
    }
}

fn header_serial(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes[4..8].try_into().unwrap())
}

fn header_count(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes[8..12].try_into().unwrap())
}

#[test]
#[ignore = "needs an X server; see the module documentation"]
fn acquiring_puts_a_readable_property_behind_the_selection() {
    let observer = Observer::new();
    assert_eq!(observer.owner(), 0, "the display must start with no manager");

    let mut manager = acquire_manager(AcquisitionPolicy::default()).expect("to acquire the selection");
    assert_eq!(
        observer.owner(),
        manager.window(),
        "a client resolving _XSETTINGS_S0 must land on the manager's window"
    );

    // Step 4 of the acquisition sequence: the property exists before
    // ownership does, so there is no moment in which a client can
    // follow the selection to a bare window.
    let initial = observer
        .property(manager.window())
        .expect("the property must exist the instant the selection resolves");
    assert_eq!(header_count(&initial), 0, "nothing published yet");

    let appearance = DesktopAppearance::new(2.0, "NeXT").with_cursor_theme("Adwaita");
    assert!(manager.publish_appearance(&appearance).unwrap());

    let published = observer.property(manager.window()).unwrap();
    assert!(header_serial(&published) > header_serial(&initial));
    assert_eq!(header_count(&published), manager.settings().len() as u32);
    assert_eq!(
        published,
        manager.settings().serialize(),
        "what the server holds must be exactly what the encoder produced"
    );
    let text = String::from_utf8_lossy(&published);
    assert!(text.contains(keys::XFT_DPI));
    assert!(text.contains("NeXT"));
}

#[test]
#[ignore = "needs an X server and gtk-query-settings; see the module documentation"]
// This probe runs on Cargo's integration-test thread, never a WM or
// compositor dispatch thread, and must finish before its output can be
// asserted.
#[allow(clippy::disallowed_methods)]
fn gtk_consumes_the_fractional_dpi_pair_and_the_physical_cursor_size() {
    let mut manager = acquire_manager(AcquisitionPolicy::default()).expect("to acquire the selection");
    assert!(manager.publish_appearance(&DesktopAppearance::new(1.5, "NeXT")).unwrap());

    let output = Command::new("gtk-query-settings").output().expect("gtk-query-settings is installed");
    assert!(
        output.status.success(),
        "gtk-query-settings exited with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let report = String::from_utf8(output.stdout).expect("gtk-query-settings writes UTF-8");
    let has = |expected: &str| report.lines().any(|line| line.trim() == expected);
    assert!(has("gtk-xft-dpi: 73728"), "GTK did not consume the 72-DPI pre-scale value:\n{report}");
    assert!(
        has("gtk-cursor-theme-size: 36"),
        "GTK must hand the already-scaled physical cursor size to Xcursor:\n{report}"
    );
}

#[test]
#[ignore = "needs an X server; see the module documentation"]
fn a_second_manager_declines_rather_than_fights() {
    let first = acquire_manager(AcquisitionPolicy::default()).expect("the first manager to win");

    match acquire_manager(AcquisitionPolicy::default()) {
        Err(XSettingsError::AlreadyOwned { selection, owner }) => {
            assert_eq!(selection, "_XSETTINGS_S0");
            assert_ne!(owner, 0, "the error must name the manager that won");
        }
        Err(other) => panic!("expected AlreadyOwned, got {other}"),
        Ok(_) => panic!("two managers must not both own the selection"),
    }

    // And the incumbent is untouched: no handover, no flicker, and its
    // window still owns the selection.
    let observer = Observer::new();
    assert_eq!(observer.owner(), first.window());
}

#[test]
#[ignore = "needs an X server; see the module documentation"]
fn a_republish_only_happens_when_something_actually_changed() {
    let mut manager = acquire_manager(AcquisitionPolicy::default()).unwrap();
    let observer = Observer::new();
    let appearance = DesktopAppearance::new(1.0, "NeXT");

    assert!(manager.publish_appearance(&appearance).unwrap());
    let after_first = observer.property(manager.window()).unwrap();

    assert!(
        !manager.publish_appearance(&appearance).unwrap(),
        "an unchanged appearance must not write the property"
    );
    assert_eq!(
        observer.property(manager.window()).unwrap(),
        after_first,
        "and must therefore not wake a single client"
    );

    // A live scale change, the case the whole crate exists for.
    assert!(manager
        .publish_appearance(&DesktopAppearance::new(2.0, "NeXT"))
        .unwrap());
    let rescaled = observer.property(manager.window()).unwrap();
    assert!(header_serial(&rescaled) > header_serial(&after_first));
    assert!(String::from_utf8_lossy(&rescaled).contains(keys::GDK_WINDOW_SCALING_FACTOR));
}

#[test]
#[ignore = "needs an X server; see the module documentation"]
fn releasing_gives_the_selection_back() {
    let manager = acquire_manager(AcquisitionPolicy::default()).unwrap();
    let observer = Observer::new();
    assert_eq!(observer.owner(), manager.window());

    manager.release().expect("a clean release");
    assert_eq!(
        observer.owner(),
        0,
        "destroying the owner window must release the selection"
    );

    // And the display is usable by the next manager, including this one
    // restarting.
    let again = acquire_manager(AcquisitionPolicy::default()).expect("to re-acquire after a release");
    assert_eq!(observer.owner(), again.window());
}

#[test]
#[ignore = "needs an X server; see the module documentation"]
fn dropping_the_manager_releases_the_selection_too() {
    let observer = Observer::new();
    {
        let manager = acquire_manager(AcquisitionPolicy::default()).unwrap();
        assert_eq!(observer.owner(), manager.window());
    }
    // Drop can only flush its best-effort destroy request; this
    // observer is a different connection and may win the scheduling
    // race to the server. Require the release, but allow that one
    // cross-connection round trip to settle.
    observer.await_owner(
        0,
        "a dropped manager must not leave a stale property behind the selection",
    );
}

#[test]
#[ignore = "needs an X server; see the module documentation"]
fn a_placeholder_owner_is_taken_over_when_the_caller_opts_in() {
    // The XWayland situation, reproduced: the selection is owned, the
    // property behind it is a valid block with zero settings, and
    // nothing will ever be published there.
    let squatter = FakeOwner::claiming(Some(&Settings::new().serialize()));
    let observer = Observer::new();
    assert_eq!(observer.owner(), squatter.window);

    let mut manager = acquire_manager(AcquisitionPolicy::TakeOverPlaceholder)
        .expect("a placeholder owner must be taken over");
    assert_eq!(
        observer.owner(),
        manager.window(),
        "the selection must now resolve to the new manager's window"
    );
    assert!(
        squatter.lost_the_selection(),
        "ICCCM's notice to the old owner is the SelectionClear the server sends it"
    );

    // And the takeover ends somewhere useful: real settings, readable
    // by a client, where the placeholder had published nothing.
    assert!(manager
        .publish_appearance(&DesktopAppearance::new(2.0, "NeXT"))
        .unwrap());
    let published = observer.property(manager.window()).unwrap();
    assert!(header_count(&published) > 0);
    assert!(String::from_utf8_lossy(&published).contains(keys::XFT_DPI));
    assert_eq!(manager.poll().unwrap(), ManagerState::Owner);
}

#[test]
#[ignore = "needs an X server; see the module documentation"]
fn a_real_owner_is_refused_even_when_takeover_is_asked_for() {
    // One setting is all it takes to be real: a manager publishing
    // anything is doing the job, and displacing it would flip that
    // setting under every client on the display.
    let mut published = Settings::new();
    assert!(published.set("Xft/DPI", 98304));
    let incumbent = FakeOwner::claiming(Some(&published.serialize()));

    match acquire_manager(AcquisitionPolicy::TakeOverPlaceholder) {
        Err(XSettingsError::AlreadyOwned { selection, owner }) => {
            assert_eq!(selection, "_XSETTINGS_S0");
            assert_eq!(owner, incumbent.window, "the error must name the manager that was respected");
        }
        Err(other) => panic!("expected AlreadyOwned, got {other}"),
        Ok(_) => panic!("a manager with settings must never be taken over"),
    }

    let observer = Observer::new();
    assert_eq!(observer.owner(), incumbent.window, "the incumbent must be untouched");
    assert_eq!(
        observer.property(incumbent.window).as_deref(),
        Some(published.serialize().as_slice()),
        "and so must its property"
    );
}

#[test]
#[ignore = "needs an X server; see the module documentation"]
fn the_default_policy_refuses_even_a_placeholder() {
    // The takeover is opt-in, and this is the test that keeps it so:
    // without the policy, an empty-block owner is refused exactly the
    // way any owner always was.
    let squatter = FakeOwner::claiming(Some(&Settings::new().serialize()));

    match acquire_manager(AcquisitionPolicy::default()) {
        Err(XSettingsError::AlreadyOwned { selection, owner }) => {
            assert_eq!(selection, "_XSETTINGS_S0");
            assert_eq!(owner, squatter.window);
        }
        Err(other) => panic!("expected AlreadyOwned, got {other}"),
        Ok(_) => panic!("the default policy must not fight anybody, placeholder or not"),
    }

    let observer = Observer::new();
    assert_eq!(observer.owner(), squatter.window);
}

#[test]
#[ignore = "needs an X server; see the module documentation"]
fn polling_an_undisturbed_manager_reports_it_still_owns_the_selection() {
    let mut manager = acquire_manager(AcquisitionPolicy::default()).unwrap();
    manager
        .publish_appearance(&DesktopAppearance::new(1.5, "NeXT"))
        .unwrap();
    assert_eq!(manager.poll().unwrap(), ManagerState::Owner);
    assert_eq!(manager.state(), ManagerState::Owner);
}
