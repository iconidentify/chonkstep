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
//! `--test-threads=1` is not optional. Every test in this file competes
//! for the *same* `_XSETTINGS_S0` selection on the same display, which
//! is precisely the resource the crate exists to hold exclusively;
//! running them concurrently would have them fail each other on purpose.

use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;

use chonk_xsettings::{DesktopAppearance, ManagerState, XSettingsError, XSettingsManager, keys};

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
        let (conn, screen) = RustConnection::connect(None).expect("a display to observe");
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

    let mut manager = XSettingsManager::acquire(None).expect("to acquire the selection");
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
#[ignore = "needs an X server; see the module documentation"]
fn a_second_manager_declines_rather_than_fights() {
    let first = XSettingsManager::acquire(None).expect("the first manager to win");

    match XSettingsManager::acquire(None) {
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
    let mut manager = XSettingsManager::acquire(None).unwrap();
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
    let manager = XSettingsManager::acquire(None).unwrap();
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
    let again = XSettingsManager::acquire(None).expect("to re-acquire after a release");
    assert_eq!(observer.owner(), again.window());
}

#[test]
#[ignore = "needs an X server; see the module documentation"]
fn dropping_the_manager_releases_the_selection_too() {
    let observer = Observer::new();
    {
        let manager = XSettingsManager::acquire(None).unwrap();
        assert_eq!(observer.owner(), manager.window());
    }
    assert_eq!(
        observer.owner(),
        0,
        "a dropped manager must not leave a stale property behind the selection"
    );
}

#[test]
#[ignore = "needs an X server; see the module documentation"]
fn polling_an_undisturbed_manager_reports_it_still_owns_the_selection() {
    let mut manager = XSettingsManager::acquire(None).unwrap();
    manager
        .publish_appearance(&DesktopAppearance::new(1.5, "NeXT"))
        .unwrap();
    assert_eq!(manager.poll().unwrap(), ManagerState::Owner);
    assert_eq!(manager.state(), ManagerState::Owner);
}
