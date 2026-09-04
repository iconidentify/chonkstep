//! XSETTINGS manager: publishing this desktop's DPI, scale, cursor and
//! theme settings to every X client at once, and updating them live.
//!
//! # The problem this exists to solve
//!
//! This desktop scales its own chrome by a UI scale factor, and it can
//! tell the applications *it* launches about that scale by putting
//! `GDK_SCALE`, `QT_SCALE_FACTOR` and `XCURSOR_SIZE` in each child's
//! environment (`chonk-shell`'s `spawn::gtk_qt_scale_env` and
//! `startup::xcursor_size_env`). Two things that mechanism cannot do:
//!
//! - **It does not reach anything the user started themselves.** A
//!   program launched from a terminal, from a shell script, from `ssh
//!   -X`, or by an application spawning a helper inherits whatever
//!   environment its parent had — which for a terminal opened before the
//!   last scale change is the old one, and for a login shell is nothing
//!   at all. Those applications render at 96 DPI on a display where
//!   everything else is at 192, which is not a subtle glitch.
//! - **It cannot update a running process.** An environment variable is
//!   read once, at startup. When the user changes the scale, every
//!   already-open window keeps the old one until it is restarted. The
//!   comment on `startup::xcursor_size_env` says exactly this about
//!   cursors and calls it a known consequence.
//!
//! XSETTINGS is the mechanism X11 desktops actually use for this, and it
//! solves both: settings live in one property on one window that *every*
//! client finds by itself, and changing that property notifies every
//! client at once. GTK, Qt and the toolkits that copied them all read
//! it. A live scale change becomes a property write, and a terminal-launched
//! application picks the settings up with no cooperation from whatever
//! started it.
//!
//! # Shape of the crate
//!
//! Two layers, split along the line of "can this be tested without an X
//! server":
//!
//! - [`mod@format`] is the wire format — the settings map and the exact
//!   bytes of the `_XSETTINGS_SETTINGS` property. Pure, no X, and tested
//!   byte-for-byte. This is where the padding and alignment rules live,
//!   and the module documentation explains why that is the part worth
//!   being frightened of.
//! - [`manager`] is the X11 side: acquiring the `_XSETTINGS_S<screen>`
//!   manager selection per ICCCM's manager convention, owning the window
//!   the property sits on, and rewriting it when something changes.
//! - [`appearance`] is the typed layer in between: a caller says "the UI
//!   scale is 2.0", not "`Xft/DPI` is 196608", and this module owns
//!   every one of the unit conversions with its factor written down.
//! - [`resources`] publishes the matching `Xft.dpi` and `Xcursor.size`
//!   declarations in the root resource database for Java, Electron and
//!   Xcursor consumers that do not implement XSETTINGS. It merges only
//!   this desktop's keys, preserving a user's other X resources.
//!
//! # Using it
//!
//! ```no_run
//! use chonk_xsettings::{DesktopAppearance, XSettingsManager, XSettingsError};
//!
//! // Acquiring may legitimately fail because some other settings
//! // daemon got there first. That is a log line, not a fatal error:
//! // the display already has a manager and the desktop should carry on.
//! let mut manager = match XSettingsManager::acquire(None) {
//!     Ok(manager) => manager,
//!     Err(error @ XSettingsError::AlreadyOwned { .. }) => {
//!         tracing::info!(%error, "not publishing XSETTINGS");
//!         return Ok(());
//!     }
//!     Err(error) => {
//!         tracing::warn!(%error, "could not start the XSETTINGS manager");
//!         return Ok(());
//!     }
//! };
//!
//! let appearance = DesktopAppearance::new(2.0, "NeXT").with_cursor_theme("Adwaita");
//! manager.publish_appearance(&appearance)?;
//!
//! // Later, when the user changes the scale — every running client is
//! // told, and the call is free if nothing actually moved.
//! let rescaled = DesktopAppearance::new(1.0, "NeXT").with_cursor_theme("Adwaita");
//! let wrote = manager.publish_appearance(&rescaled)?;
//! assert!(wrote);
//! # Ok::<(), XSettingsError>(())
//! ```
//!
//! # What this does not do
//!
//! Only the *manager* side. Reading another desktop's settings — the
//! client half of the specification — is not here, because this desktop
//! has no use for it: it is the desktop, and if something else already
//! owns the selection the right answer is to leave that manager alone
//! (see [`manager`]'s "Losing gracefully"), not to start following its
//! opinions. One carve-out exists, because XWayland forced the issue:
//! an owner that is publishing *nothing* — XWayland claims the
//! selection at startup and puts an empty settings block behind it —
//! can be taken over via [`manager::AcquisitionPolicy`], and the only
//! parsing this crate does of anyone else's property is the header
//! check that decides it ([`format::parse_header`]).
//!
//! Nor does it replace the per-child environment variables. The two
//! overlap but neither subsumes the other: XSETTINGS reaches every X
//! client including the ones this desktop did not launch, while the
//! environment reaches non-X11 processes and toolkits that read a
//! variable before they ever open a display connection. Publishing both,
//! consistently, is why [`appearance`] derives its cursor size from the
//! same 24-pixel base `chonk-shell` does.

#![deny(missing_docs)]

pub mod appearance;
pub mod format;
pub mod manager;
pub mod resources;

pub use appearance::{
    BASE_CURSOR_SIZE_PX, BASE_DPI, DesktopAppearance, MAX_UI_SCALE, MIN_UI_SCALE,
    UNSCALED_XFT_DPI, XFT_DPI_UNITS_PER_POINT, cursor_size_for_scale, keys, sanitize_ui_scale,
    window_scaling_factor_for_scale, xft_dpi_for_scale,
};
pub use format::{
    ByteOrder, Header, MAX_NAME_BYTES, MAX_STRING_BYTES, SettingValue, Settings, parse_header,
    serialize,
};
pub use manager::{AcquisitionPolicy, ManagerState, XSettingsError, XSettingsManager};
pub use resources::merge_resource_manager;
