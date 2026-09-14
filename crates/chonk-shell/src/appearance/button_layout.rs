//! Window buttons for applications that draw their own titlebar.
//!
//! A GTK or libadwaita header bar lays out its window buttons from
//! `org.gnome.desktop.wm.preferences button-layout`, whose stock value is
//! `appmenu:close`. On this desktop that is a header bar with a lone Close
//! button, although the compositor advertises minimize and maximize in
//! `xdg_toplevel.wm_capabilities` and carries out both. So the session
//! publishes `appmenu:minimize,maximize,close` instead. Sandboxed
//! applications read the same key, republished by the settings portal.
//!
//! Only the stock value is ever replaced, and only while it is the stock
//! value by default rather than by choice. When `dconf` can tell, a user
//! database holding `appmenu:close` explicitly belongs to a user who chose
//! Close alone, and it is left alone. Any other layout is the user's and
//! is never touched, and a layout an earlier session published is
//! recognised and not written again.

use std::time::Duration;

const SCHEMA: &str = "org.gnome.desktop.wm.preferences";
const KEY: &str = "button-layout";
const DCONF_KEY: &str = "/org/gnome/desktop/wm/preferences/button-layout";
/// The layout the schema ships.
pub(crate) const STOCK_LAYOUT: &str = "appmenu:close";
/// The layout this session publishes: every button the compositor honors.
pub(crate) const SESSION_LAYOUT: &str = "appmenu:minimize,maximize,close";

/// Where the layout is read from and written to. The real store runs
/// `gsettings` and `dconf`; tests hand in their own, so no test can reach
/// the preferences of the user running it.
pub(crate) trait PreferenceStore {
    /// The effective layout, as GSettings prints it, or `None` when it
    /// cannot be read at all.
    fn layout(&mut self) -> Option<String>;
    /// Whether the user's own database holds a value for the key, rather
    /// than the effective value being the schema's; `None` when that
    /// cannot be told.
    fn user_has_layout(&mut self) -> Option<bool>;
    /// Writes a layout, answering whether it took.
    fn set_layout(&mut self, layout: &str) -> bool;
}

/// What publishing did, for the one log line it earns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Published,
    AlreadyPublished,
    KeptUserLayout(String),
    Unreadable,
    WriteFailed,
}

/// Publishes [`SESSION_LAYOUT`] to `store` if, and only if, the layout
/// there is the untouched stock one.
pub(crate) fn publish_to(store: &mut impl PreferenceStore) -> Outcome {
    let Some(current) = store.layout() else {
        return Outcome::Unreadable;
    };
    let current = unquote(&current);
    if current == SESSION_LAYOUT {
        return Outcome::AlreadyPublished;
    }
    if current != STOCK_LAYOUT || store.user_has_layout() == Some(true) {
        return Outcome::KeptUserLayout(current.to_owned());
    }
    if store.set_layout(SESSION_LAYOUT) {
        Outcome::Published
    } else {
        Outcome::WriteFailed
    }
}

/// GSettings and dconf print a string as a quoted GVariant,
/// `'appmenu:close'`.
fn unquote(text: &str) -> &str {
    let text = text.trim();
    text.strip_prefix('\'').and_then(|inner| inner.strip_suffix('\'')).unwrap_or(text)
}

/// The user's real preferences, through the command-line tools.
struct Gsettings;

impl Gsettings {
    fn run(program: &str, args: &[&str]) -> Option<String> {
        let child = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;
        crate::spawn::wait_with_deadline(child, program, Duration::from_secs(2))
    }
}

impl PreferenceStore for Gsettings {
    fn layout(&mut self) -> Option<String> {
        Self::run("gsettings", &["get", SCHEMA, KEY])
    }

    fn user_has_layout(&mut self) -> Option<bool> {
        // `dconf read` prints nothing for a key the user database does
        // not hold, and is absent where GSettings has another backend.
        Self::run("dconf", &["read", DCONF_KEY]).map(|value| !value.trim().is_empty())
    }

    fn set_layout(&mut self, layout: &str) -> bool {
        Self::run("gsettings", &["set", SCHEMA, KEY, layout]).is_some()
    }
}

/// Publishes the session's window button layout, once, off the main
/// thread: the tools it runs are allowed two seconds each, and a session
/// must not wait on them to draw its first frame.
pub fn publish_button_layout() {
    // The guard the color scheme has, for the same reason: a posed nested
    // session must not rewrite the developer's own preferences, and the
    // test harness sets this on every session it boots. A unit test build
    // never publishes at all.
    if cfg!(test) || std::env::var_os("CHONKSTEP_NO_APPEARANCE_PROPAGATION").is_some() {
        tracing::info!("window button layout publishing disabled for this session");
        return;
    }
    let spawned = std::thread::Builder::new().name("button-layout".into()).spawn(|| match publish_to(&mut Gsettings) {
        Outcome::Published => tracing::info!(
            layout = SESSION_LAYOUT,
            "published window buttons for applications that draw their own titlebar"
        ),
        Outcome::AlreadyPublished => tracing::debug!(layout = SESSION_LAYOUT, "window button layout already published"),
        Outcome::KeptUserLayout(layout) => tracing::info!(%layout, "kept the user's own window button layout"),
        Outcome::Unreadable => tracing::warn!(
            "gsettings unavailable or schema missing; header bars keep their stock window buttons"
        ),
        Outcome::WriteFailed => tracing::warn!(layout = SESSION_LAYOUT, "could not publish the window button layout"),
    });
    if let Err(error) = spawned {
        tracing::warn!(?error, "could not start the window button layout publisher");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for the user's preferences that records every write.
    struct Store {
        layout: Option<String>,
        user_set: Option<bool>,
        writable: bool,
        writes: Vec<String>,
    }

    impl Store {
        fn holding(layout: &str, user_set: Option<bool>) -> Self {
            Self { layout: Some(format!("'{layout}'\n")), user_set, writable: true, writes: Vec::new() }
        }
    }

    impl PreferenceStore for Store {
        fn layout(&mut self) -> Option<String> {
            self.layout.clone()
        }

        fn user_has_layout(&mut self) -> Option<bool> {
            self.user_set
        }

        fn set_layout(&mut self, layout: &str) -> bool {
            if self.writable {
                self.writes.push(layout.to_owned());
                self.layout = Some(format!("'{layout}'\n"));
                self.user_set = Some(true);
            }
            self.writable
        }
    }

    #[test]
    fn the_stock_layout_gains_minimize_and_maximize_once() {
        // Whether dconf says the value is the schema's, or cannot say.
        for user_set in [Some(false), None] {
            let mut store = Store::holding(STOCK_LAYOUT, user_set);
            assert_eq!(publish_to(&mut store), Outcome::Published, "user_set={user_set:?}");
            assert_eq!(store.writes, vec![SESSION_LAYOUT.to_owned()]);
            // The next session finds it already there and writes nothing.
            assert_eq!(publish_to(&mut store), Outcome::AlreadyPublished);
            assert_eq!(store.writes.len(), 1);
        }
    }

    #[test]
    fn a_layout_the_user_chose_is_never_overwritten() {
        for custom in ["close,minimize,maximize:", ":close", "appmenu:minimize,close", "icon:close"] {
            for user_set in [Some(true), Some(false), None] {
                let mut store = Store::holding(custom, user_set);
                assert_eq!(publish_to(&mut store), Outcome::KeptUserLayout(custom.to_owned()));
                assert!(store.writes.is_empty(), "{custom} was overwritten");
            }
        }
        // Close alone, written into the user's database on purpose rather
        // than inherited from the schema.
        let mut store = Store::holding(STOCK_LAYOUT, Some(true));
        assert_eq!(publish_to(&mut store), Outcome::KeptUserLayout(STOCK_LAYOUT.to_owned()));
        assert!(store.writes.is_empty());
    }

    #[test]
    fn missing_tools_or_a_refused_write_change_nothing() {
        let mut store = Store { layout: None, ..Store::holding(STOCK_LAYOUT, None) };
        assert_eq!(publish_to(&mut store), Outcome::Unreadable);
        assert!(store.writes.is_empty());
        let mut store = Store { writable: false, ..Store::holding(STOCK_LAYOUT, None) };
        assert_eq!(publish_to(&mut store), Outcome::WriteFailed);
        assert!(store.writes.is_empty());
    }

    #[test]
    fn gvariant_quoting_is_read_through() {
        assert_eq!(unquote("'appmenu:close'\n"), "appmenu:close");
        assert_eq!(unquote("appmenu:close"), "appmenu:close");
        assert_eq!(unquote("''"), "");
    }
}
