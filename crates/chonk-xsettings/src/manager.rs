//! The X11 half: owning the `_XSETTINGS_S<screen>` manager selection
//! and keeping the property on the owner window up to date.
//!
//! Everything here is a thin shell around [`crate::format`]. The bytes
//! are decided there and tested there; this module's job is the ICCCM
//! choreography that makes a client look at them, and doing that
//! choreography in the order that leaves no window in which a client can
//! observe an inconsistent state.
//!
//! # The manager-selection convention (ICCCM §2.8)
//!
//! XSETTINGS does not invent a discovery mechanism. It reuses the one
//! ICCCM defines for "manager" programs: a manager owns an X selection
//! named after the thing it manages and the screen it manages it on,
//! and every client finds the manager by asking who owns that
//! selection. For XSETTINGS the name is `_XSETTINGS_S<screen>` — screen
//! 0 on a single-head display gives `_XSETTINGS_S0` — and the owner
//! window is also the window the settings property lives on.
//!
//! The sequence [`XSettingsManager::acquire`] performs, and why each
//! step is where it is:
//!
//! 1. **Ask who owns the selection.** If anybody does, stop. See
//!    "Losing gracefully" below.
//! 2. **Create the owner window.** Unmapped, 1×1, `override-redirect`,
//!    off-screen. It is never drawn; it exists to be a name a client can
//!    hang a `PropertyNotify` selection on and to die with this process.
//! 3. **Get a real timestamp.** ICCCM says not to pass `CurrentTime` to
//!    `SetSelectionOwner`, because the server substitutes its own clock
//!    and the client can no longer reason about ordering against a
//!    competitor. The standard way to obtain one without a user event is
//!    to change a property on your own window and read the time out of
//!    the resulting `PropertyNotify` — which is what this does, using
//!    the `WM_NAME` it wants to set anyway so that `xprop` on the owner
//!    window identifies the manager.
//! 4. **Write `_XSETTINGS_SETTINGS` — before taking ownership.** This
//!    ordering is the one thing in this module that is easy to get
//!    backwards and impossible to notice in testing. A client that is
//!    watching for the manager announcement in step 6 will read the
//!    property the instant it sees it; if ownership came first, there
//!    would be a window in which the selection resolves to a window with
//!    no property on it, and a client that lost that race would sit with
//!    default settings until something else changed.
//! 5. **Take the selection, then read the owner back.** The read-back is
//!    not paranoia: between steps 1 and 5 another manager may have
//!    started, and `SetSelectionOwner` succeeds regardless — the *last*
//!    writer wins. Verifying is the only way to know which one this
//!    process was.
//! 6. **Announce.** A `MANAGER` client message to the root window tells
//!    clients that were already running, and were watching for exactly
//!    this, to go and read the property. Clients that start later find
//!    the selection by asking, so the announcement is only for the ones
//!    already there — but without it, every application open at login
//!    keeps its defaults until it happens to restart.
//!
//! # Losing gracefully
//!
//! Another XSETTINGS manager already owning the selection is a normal
//! thing to find, not an error in this desktop: `xsettingsd`, a leftover
//! GNOME or XFCE settings daemon, or a second copy of this session all
//! produce it. There is a mechanism in ICCCM for taking a selection away
//! from its current owner by force, and this crate deliberately does not
//! use it. Two managers fighting over the selection is worse than either
//! one winning — clients see the settings flip on every handover — and
//! the other manager is, by construction, already publishing DPI and
//! theme settings that work. So [`XSettingsError::AlreadyOwned`] comes
//! back, the caller logs it and carries on with the rest of the desktop,
//! and the X clients keep the settings they had.
//!
//! The mirror image is handled too: if another manager takes the
//! selection away from *this* one later, the server sends a
//! `SelectionClear`, and [`XSettingsManager::poll`] latches that into
//! [`ManagerState::Superseded`] and stops writing the property. ICCCM
//! requires the old owner to stand down; continuing to write would put
//! two processes' settings on two different windows, with clients
//! reading whichever one they found first.
//!
//! # The connection is owned, on purpose
//!
//! [`XSettingsManager`] takes ownership of its `Connection` rather than
//! borrowing the one the window manager already has. That is not
//! tidiness, it is the only arrangement that works: acquiring the
//! selection requires *reading events* (the `PropertyNotify` in step 3),
//! and staying a good selection owner requires reading more of them
//! (`SelectionClear`, `SelectionRequest`). An X connection has one event
//! queue, so a library that reads events off a connection it shares with
//! a window manager's main loop is a library that eats that main loop's
//! `MapRequest`s. One extra connection to the display costs a socket and
//! removes the entire class of bug.

use x11rb::connection::Connection;
use x11rb::errors::{ConnectError, ConnectionError, ReplyError, ReplyOrIdError};
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, CLIENT_MESSAGE_EVENT, ClientMessageEvent, ConnectionExt as _, CreateWindowAux,
    EventMask, PropMode, SELECTION_NOTIFY_EVENT, SelectionNotifyEvent, SelectionRequestEvent,
    Timestamp, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT, NONE};

use crate::appearance::DesktopAppearance;
use crate::format::Settings;

/// The `WM_NAME` put on the owner window.
///
/// Nothing in the protocol reads it. It is there because the owner
/// window is otherwise an anonymous 1×1 rectangle that never appears on
/// screen, and the first thing anybody debugging "why do my fonts look
/// like that" does is `xprop -root _XSETTINGS_S0` and follow the window
/// id — at which point a name is the difference between an answer and
/// another twenty minutes. It doubles as the property write that yields
/// a selection timestamp; see the module documentation, step 3.
const OWNER_WINDOW_NAME: &[u8] = b"chonkstep XSETTINGS manager";

/// Everything that can go wrong acquiring or publishing settings.
///
/// The X variants wrap `x11rb`'s errors verbatim and exist so a caller
/// can tell "the display went away" from "somebody else is the settings
/// manager", which are the two failures with genuinely different
/// responses: the first ends the session, the second is a shrug.
#[derive(Debug, thiserror::Error)]
pub enum XSettingsError {
    /// Opening a connection to the display failed.
    #[error("failed to connect to the X display: {0}")]
    Connect(#[from] ConnectError),
    /// The connection broke, or a request could not be written.
    #[error("X11 connection error: {0}")]
    Connection(#[from] ConnectionError),
    /// A request came back as an X error.
    #[error("X11 request failed: {0}")]
    Reply(#[from] ReplyError),
    /// The server would not allocate a resource id.
    #[error("X11 id allocation failed: {0}")]
    ReplyOrId(#[from] ReplyOrIdError),
    /// Another XSETTINGS manager already owns the selection, or took it
    /// during acquisition.
    ///
    /// The expected, benign failure. Log it and continue: the other
    /// manager is publishing settings, this one is not needed, and
    /// forcing the issue would only make clients flicker between two
    /// sets of settings. See "Losing gracefully" in the module
    /// documentation.
    #[error("another XSETTINGS manager already owns {selection} (window {owner:#x}); leaving it alone")]
    AlreadyOwned {
        /// The selection name, e.g. `_XSETTINGS_S0`.
        selection: String,
        /// The X window id of the manager that owns it, or `0` when the
        /// owner vanished between the two round trips.
        owner: Window,
    },
    /// This manager owned the selection and lost it to another client.
    ///
    /// Returned from [`XSettingsManager::update`] once
    /// [`XSettingsManager::poll`] has seen the `SelectionClear`. Not a
    /// failure to recover from: the correct response is to drop the
    /// manager.
    #[error("lost the {0} manager selection to another client")]
    SelectionLost(String),
    /// The requested screen number does not exist on this display.
    #[error("screen {requested} does not exist; this display has {available}")]
    NoSuchScreen {
        /// The screen number the caller asked for.
        requested: usize,
        /// How many screens the display actually has.
        available: usize,
    },
    /// The X server never sent the `PropertyNotify` the selection
    /// timestamp is read from.
    ///
    /// Only reachable if the connection dies mid-acquisition, since the
    /// event is guaranteed by the protocol once `PropertyChange` is
    /// selected. It is a distinct variant rather than a panic because a
    /// dying display is not this crate's bug to assert about.
    #[error("the X server produced no timestamp for the selection")]
    NoTimestamp,
}

/// Whether this manager still owns the selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagerState {
    /// This process owns `_XSETTINGS_S<screen>` and its property is the
    /// one clients read.
    Owner,
    /// Another client took the selection. This manager has stood down
    /// and will refuse further writes; see the module documentation.
    Superseded,
}

/// An XSETTINGS manager: owns the selection, owns the window, owns the
/// settings map.
///
/// Generic over the connection so a caller can supply one it made
/// itself, but note the module documentation: whatever is passed in is
/// *consumed*, and its event queue is read by this type. Do not hand
/// over a connection anything else is polling.
///
/// Dropping the manager destroys the owner window, which releases the
/// selection and, as a side effect, deletes the property — so the
/// settings this desktop published cease to exist along with the desktop
/// that meant them. That is the correct behaviour: a stale
/// `_XSETTINGS_SETTINGS` on a window nobody owns would be read by the
/// next client to start and never updated again.
pub struct XSettingsManager<C: Connection = RustConnection> {
    conn: C,
    screen_num: usize,
    root: Window,
    window: Window,
    selection: Atom,
    selection_name: String,
    settings_atom: Atom,
    timestamp_atom: Atom,
    /// The timestamp ownership was taken at. Kept because ICCCM makes it
    /// the answer to a `TIMESTAMP` conversion request — see
    /// [`XSettingsManager::poll`].
    acquired_at: Timestamp,
    settings: Settings,
    state: ManagerState,
    /// Set by [`XSettingsManager::release`] so [`Drop`] does not repeat
    /// the teardown it already did (and already reported errors for).
    released: bool,
}

impl XSettingsManager<RustConnection> {
    /// Opens a dedicated connection to the display and acquires the
    /// settings manager selection on the screen that display name
    /// selects.
    ///
    /// `display_name` of `None` means `$DISPLAY`, with the screen number
    /// taken from it — so `:0.1` acquires `_XSETTINGS_S1`. That is the
    /// right default: the screen a session is running on is the screen
    /// whose settings it has any business publishing.
    ///
    /// The connection this opens is separate from any the caller already
    /// has, deliberately; see the module documentation.
    pub fn acquire(display_name: Option<&str>) -> Result<Self, XSettingsError> {
        let (conn, screen_num) = RustConnection::connect(display_name)?;
        Self::acquire_with_connection(conn, screen_num)
    }
}

impl<C: Connection> XSettingsManager<C> {
    /// Acquires the selection on `screen_num` using a connection the
    /// caller supplies and this type takes over.
    ///
    /// Every event on `conn` from here on belongs to this manager; see
    /// the module documentation for why that has to be true.
    pub fn acquire_with_connection(conn: C, screen_num: usize) -> Result<Self, XSettingsError> {
        let available = conn.setup().roots.len();
        let root = conn
            .setup()
            .roots
            .get(screen_num)
            .ok_or(XSettingsError::NoSuchScreen {
                requested: screen_num,
                available,
            })?
            .root;

        let selection_name = selection_name_for_screen(screen_num);
        let selection = intern(&conn, selection_name.as_bytes())?;
        let settings_atom = intern(&conn, b"_XSETTINGS_SETTINGS")?;
        let manager_atom = intern(&conn, b"MANAGER")?;
        let timestamp_atom = intern(&conn, b"TIMESTAMP")?;

        // Step 1: is somebody already doing this job? Asked before any
        // resource is created, so the common "xsettingsd is running"
        // case costs one round trip and leaves nothing behind.
        let existing = conn.get_selection_owner(selection)?.reply()?.owner;
        if existing != NONE {
            return Err(XSettingsError::AlreadyOwned {
                selection: selection_name,
                owner: existing,
            });
        }

        // Step 2: the owner window. Off-screen, 1x1, override-redirect
        // and never mapped — override-redirect so that a window manager
        // which is not this desktop cannot decide to reparent or
        // decorate it, and `PropertyChange` selected because step 3
        // reads a `PropertyNotify` off it.
        //
        // InputOutput rather than InputOnly. An InputOnly window can
        // hold properties and would do, but every XSETTINGS manager
        // that clients have ever been tested against uses a normal
        // window, and this is not the place to find out which client
        // assumed that.
        let window = conn.generate_id()?;
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            root,
            -1,
            -1,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            COPY_FROM_PARENT,
            &CreateWindowAux::new()
                .override_redirect(1)
                .event_mask(EventMask::PROPERTY_CHANGE),
        )?
        .check()?;

        let settings = Settings::new();
        match Self::take_selection(
            &conn,
            root,
            window,
            selection,
            settings_atom,
            manager_atom,
            &settings,
        ) {
            Ok(acquired_at) => {
                tracing::info!(
                    selection = %selection_name,
                    window,
                    acquired_at,
                    "acquired the XSETTINGS manager selection"
                );
                Ok(Self {
                    conn,
                    screen_num,
                    root,
                    window,
                    selection,
                    selection_name,
                    settings_atom,
                    timestamp_atom,
                    acquired_at,
                    settings,
                    state: ManagerState::Owner,
                    released: false,
                })
            }
            Err(error) => {
                // The window is useless now and would otherwise live
                // until the connection closes. Best effort: the caller
                // is already being handed the interesting error and a
                // failure to clean up after a failure is not worth
                // replacing it with.
                if let Err(cleanup) = conn.destroy_window(window) {
                    tracing::debug!(?cleanup, "could not destroy the owner window after a failed acquisition");
                }
                let _ = conn.flush();
                Err(match error {
                    XSettingsError::AlreadyOwned { owner, .. } => XSettingsError::AlreadyOwned {
                        selection: selection_name,
                        owner,
                    },
                    other => other,
                })
            }
        }
    }

    /// Steps 3 to 6 of the acquisition sequence, factored out so that
    /// the caller above has one error path to clean the window up on.
    /// Returns the timestamp ownership was taken at.
    fn take_selection(
        conn: &C,
        root: Window,
        window: Window,
        selection: Atom,
        settings_atom: Atom,
        manager_atom: Atom,
        settings: &Settings,
    ) -> Result<Timestamp, XSettingsError> {
        // Step 3. Setting `WM_NAME` is a property change on our own
        // window, so the server answers with a `PropertyNotify` whose
        // `time` is a timestamp from its own clock — which is exactly
        // what ICCCM wants passed to `SetSelectionOwner`, and what
        // `CurrentTime` would deny us.
        conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            OWNER_WINDOW_NAME,
        )?
        .check()?;
        let timestamp = Self::await_timestamp(conn, window)?;

        // Step 4: the property goes on before the selection does, so
        // that no client can ever resolve the selection to a window
        // with nothing on it. See the module documentation.
        conn.change_property8(
            PropMode::REPLACE,
            window,
            settings_atom,
            settings_atom,
            &settings.serialize(),
        )?
        .check()?;

        // Step 5, and the read-back that makes it meaningful.
        conn.set_selection_owner(window, selection, timestamp)?
            .check()?;
        let owner = conn.get_selection_owner(selection)?.reply()?.owner;
        if owner != window {
            return Err(XSettingsError::AlreadyOwned {
                selection: String::new(), // filled in by the caller, which has the name
                owner,
            });
        }

        // Step 6. `data[0]` is the timestamp, `data[1]` the selection
        // atom, `data[2]` the owner window: the layout ICCCM specifies
        // for a manager announcement, and the one clients parse.
        let announcement = ClientMessageEvent {
            response_type: CLIENT_MESSAGE_EVENT,
            format: 32,
            sequence: 0,
            window: root,
            type_: manager_atom,
            data: [timestamp, selection, window, 0, 0].into(),
        };
        conn.send_event(false, root, EventMask::STRUCTURE_NOTIFY, announcement)?
            .check()?;
        conn.flush()?;

        Ok(timestamp)
    }

    /// Reads events until the `PropertyNotify` from the `WM_NAME` write
    /// arrives, and returns its timestamp.
    ///
    /// Blocking is acceptable here and only here: the connection is this
    /// manager's own (see the module documentation), the request that
    /// causes the event has already been flushed by `check()`, and the
    /// protocol guarantees the event because `PropertyChange` was
    /// selected at window creation. Anything else that arrives first is
    /// discarded — on a connection this new there should be nothing, and
    /// a stray event is not worth failing over.
    fn await_timestamp(conn: &C, window: Window) -> Result<Timestamp, XSettingsError> {
        loop {
            match conn.wait_for_event() {
                Ok(Event::PropertyNotify(event))
                    if event.window == window && event.atom == u32::from(AtomEnum::WM_NAME) =>
                {
                    return Ok(event.time);
                }
                Ok(other) => {
                    tracing::trace!(?other, "discarding an event while waiting for a timestamp");
                }
                Err(error) => {
                    tracing::warn!(?error, "the display died while acquiring a selection timestamp");
                    return Err(XSettingsError::NoTimestamp);
                }
            }
        }
    }

    /// The settings currently published.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Whether this manager still owns the selection, as of the last
    /// [`poll`](Self::poll).
    pub fn state(&self) -> ManagerState {
        self.state
    }

    /// The owner window — the one clients read `_XSETTINGS_SETTINGS`
    /// from and watch for `PropertyNotify`.
    pub fn window(&self) -> Window {
        self.window
    }

    /// The root window of the screen this manager serves.
    pub fn root(&self) -> Window {
        self.root
    }

    /// The screen number this manager serves.
    pub fn screen_number(&self) -> usize {
        self.screen_num
    }

    /// The interned `_XSETTINGS_S<screen>` atom.
    pub fn selection_atom(&self) -> Atom {
        self.selection
    }

    /// The selection's name, e.g. `_XSETTINGS_S0`. Worth logging.
    pub fn selection_name(&self) -> &str {
        &self.selection_name
    }

    /// The connection this manager owns, for a caller that wants its
    /// file descriptor to add to a poll set — see [`poll`](Self::poll).
    pub fn connection(&self) -> &C {
        &self.conn
    }

    /// Edits the settings and rewrites the property if anything changed.
    ///
    /// Returns whether the property was written. The check is the
    /// serial: [`Settings::set`](crate::format::Settings::set) only
    /// moves it when a value really changed, so a caller can re-apply
    /// its whole configuration on every reload and only pay for an X
    /// round trip when something moved. That matters more than it
    /// sounds: writing the property wakes every client on the display
    /// with a `PropertyNotify`, and a GTK application answers one by
    /// re-reading its settings and, for a DPI change, re-laying out
    /// every window it has.
    ///
    /// The edit closure runs before ownership is re-checked, so an edit
    /// made after the selection was lost still updates the in-memory map
    /// — the map stays a truthful record of what this desktop *wants* —
    /// but nothing is written and [`XSettingsError::SelectionLost`]
    /// comes back.
    pub fn update(&mut self, edit: impl FnOnce(&mut Settings)) -> Result<bool, XSettingsError> {
        let before = self.settings.serial();
        edit(&mut self.settings);
        if self.settings.serial() == before {
            return Ok(false);
        }
        if self.state == ManagerState::Superseded {
            return Err(XSettingsError::SelectionLost(self.selection_name.clone()));
        }

        self.write_property()?;
        tracing::debug!(
            serial = self.settings.serial(),
            settings = self.settings.len(),
            "published XSETTINGS"
        );
        Ok(true)
    }

    /// Publishes a whole [`DesktopAppearance`], writing the property
    /// only if it differs from what is already out there.
    ///
    /// This is the call the desktop makes on a scale or theme change,
    /// and the one it can also make unconditionally at startup and on
    /// every config reload.
    pub fn publish_appearance(
        &mut self,
        appearance: &DesktopAppearance,
    ) -> Result<bool, XSettingsError> {
        self.update(|settings| {
            appearance.apply_to(settings);
        })
    }

    /// Handles the selection events the X server sends this manager, and
    /// reports whether it still owns the selection.
    ///
    /// Non-blocking; drains whatever has arrived and returns. A caller
    /// should call it whenever [`connection`](Self::connection)'s file
    /// descriptor is readable, or failing that on whatever timer it
    /// already has — nothing breaks if it is called rarely, it just
    /// delays noticing that another manager took over.
    ///
    /// Two kinds of event matter:
    ///
    /// - `SelectionClear` means another client took the selection. The
    ///   state latches to [`ManagerState::Superseded`] and this manager
    ///   stops writing, as ICCCM requires of a former owner.
    /// - `SelectionRequest` is a client asking to *convert* the
    ///   selection. Manager selections rarely receive one, but a
    ///   requestor that gets no answer does not fail fast — it waits out
    ///   its own timeout, which in practice is a client that appears to
    ///   hang. `TIMESTAMP` is answered with the timestamp ownership was
    ///   taken at, as ICCCM defines; everything else is refused
    ///   explicitly, which is a valid answer and an immediate one.
    pub fn poll(&mut self) -> Result<ManagerState, XSettingsError> {
        while let Some(event) = self.conn.poll_for_event()? {
            match event {
                Event::SelectionClear(event)
                    if event.selection == self.selection && event.owner == self.window =>
                {
                    tracing::warn!(
                        selection = %self.selection_name,
                        "another client took the XSETTINGS manager selection; standing down"
                    );
                    self.state = ManagerState::Superseded;
                }
                Event::SelectionRequest(event) if event.selection == self.selection => {
                    self.answer_selection_request(&event)?;
                }
                other => {
                    tracing::trace!(?other, "ignoring an event on the XSETTINGS connection");
                }
            }
        }
        Ok(self.state)
    }

    /// Answers one `SelectionRequest`: the `TIMESTAMP` target, or a
    /// refusal.
    ///
    /// A refusal is a `SelectionNotify` with `property` set to `None`,
    /// which is the protocol's word for "I will not convert to that" —
    /// as opposed to silence, which the requestor cannot distinguish
    /// from a manager that has crashed until its timeout expires.
    fn answer_selection_request(
        &self,
        request: &SelectionRequestEvent,
    ) -> Result<(), XSettingsError> {
        // ICCCM: an obsolete requestor may send `property` as `None`,
        // and the owner is to use the target atom as the property name.
        let property = if request.property == NONE {
            request.target
        } else {
            request.property
        };

        let answered = if request.target == self.timestamp_atom {
            // ICCCM types the TIMESTAMP conversion as INTEGER.
            let write = self
                .conn
                .change_property32(
                    PropMode::REPLACE,
                    request.requestor,
                    property,
                    AtomEnum::INTEGER,
                    &[self.acquired_at],
                )
                .map_err(XSettingsError::from)
                .and_then(|cookie| cookie.check().map_err(XSettingsError::from));
            match write {
                Ok(()) => true,
                Err(error) => {
                    // The requestor may well have died between sending
                    // the request and now, which turns the write into an
                    // X error about a window that no longer exists. Not
                    // worth propagating; the refusal below is still a
                    // correct answer.
                    tracing::debug!(?error, "could not write a TIMESTAMP conversion to the requestor");
                    false
                }
            }
        } else {
            tracing::debug!(
                conversion_target = request.target,
                "refusing an XSETTINGS selection conversion this manager does not implement"
            );
            false
        };

        let notify = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: request.time,
            requestor: request.requestor,
            selection: request.selection,
            target: request.target,
            property: if answered { property } else { NONE },
        };
        // Sent with an empty event mask, as ICCCM specifies for
        // `SelectionNotify`. Errors are logged rather than returned for
        // the same reason as above: a dead requestor must not take the
        // settings manager down with it.
        if let Err(error) = self
            .conn
            .send_event(false, request.requestor, EventMask::NO_EVENT, notify)
        {
            tracing::debug!(?error, "could not answer a selection request");
        }
        self.conn.flush()?;
        Ok(())
    }

    /// Writes the current settings to `_XSETTINGS_SETTINGS`.
    ///
    /// Both the property and its type are the `_XSETTINGS_SETTINGS`
    /// atom, at format 8 — the specification's requirement, and the
    /// reason the format carries its own byte-order byte: at format 8
    /// the server does no swapping of its own.
    ///
    /// The write is `check()`ed. That costs a round trip on a path that
    /// runs once per user-visible settings change, and it buys the
    /// difference between a property that failed to write and a desktop
    /// that thinks it published something.
    fn write_property(&self) -> Result<(), XSettingsError> {
        self.conn
            .change_property8(
                PropMode::REPLACE,
                self.window,
                self.settings_atom,
                self.settings_atom,
                &self.settings.serialize(),
            )?
            .check()?;
        self.conn.flush()?;
        Ok(())
    }

    /// Releases the selection and reports whether the teardown worked.
    ///
    /// Destroying the owner window is what releases the selection —
    /// atomically, and without a moment in which the selection points at
    /// a window whose property has already been deleted. [`Drop`] does
    /// the same thing; this exists only for a caller that wants to know
    /// whether it succeeded.
    pub fn release(mut self) -> Result<(), XSettingsError> {
        self.released = true;
        self.conn.destroy_window(self.window)?.check()?;
        self.conn.flush()?;
        tracing::info!(selection = %self.selection_name, "released the XSETTINGS manager selection");
        Ok(())
    }
}

impl<C: Connection> Drop for XSettingsManager<C> {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        // Best effort, and quiet about it: by the time a manager is
        // being dropped the usual reason is that the session is ending,
        // in which case the display may already be gone and there is
        // nobody left to tell.
        if let Err(error) = self.conn.destroy_window(self.window) {
            tracing::debug!(?error, "could not destroy the XSETTINGS owner window");
        }
        let _ = self.conn.flush();
    }
}

/// The ICCCM manager-selection name for a screen.
///
/// Exact, and unforgivingly so: a client looks the manager up by this
/// literal string, so an off-by-one in the screen number or a stray
/// character produces a manager that acquires a selection nobody is
/// watching and publishes settings nobody reads — with no error
/// anywhere. Hence a named function with a test rather than a `format!`
/// buried in the acquisition path.
fn selection_name_for_screen(screen_num: usize) -> String {
    format!("_XSETTINGS_S{screen_num}")
}

/// Interns one atom, with `only_if_exists` false because every atom this
/// crate uses is one it may well be the first to name.
fn intern<C: Connection>(conn: &C, name: &[u8]) -> Result<Atom, XSettingsError> {
    Ok(conn.intern_atom(false, name)?.reply()?.atom)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The selection name is the one thing in this module that is pure
    /// string handling, and it is also the one thing that silently
    /// produces a manager nobody can find if it is wrong: a client looks
    /// up `_XSETTINGS_S0` by exact name.
    #[test]
    fn the_selection_is_named_after_the_screen() {
        assert_eq!(selection_name_for_screen(0), "_XSETTINGS_S0");
        assert_eq!(selection_name_for_screen(1), "_XSETTINGS_S1");
        assert_eq!(selection_name_for_screen(12), "_XSETTINGS_S12");
    }

    #[test]
    fn the_already_owned_error_names_the_selection_and_the_owner() {
        let error = XSettingsError::AlreadyOwned {
            selection: "_XSETTINGS_S0".to_string(),
            owner: 0x0140_0003,
        };
        let text = error.to_string();
        assert!(text.contains("_XSETTINGS_S0"), "{text}");
        assert!(text.contains("0x1400003"), "{text}");
    }

    #[test]
    fn losing_the_selection_is_a_distinguishable_error() {
        let error = XSettingsError::SelectionLost("_XSETTINGS_S0".to_string());
        assert!(error.to_string().contains("_XSETTINGS_S0"));
    }
}
