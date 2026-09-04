use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};

use wm_core::{
    Backend, BackendEvent, DragHandle, KeyCombo, Modifiers, MonitorInfo, MouseButton, NetState,
    NetStateAction, ScrollDelta, SizeHints, SurfaceRef, WindowType, WmClass, WmProtocol,
    MAX_WORKSPACES,
};
use wm_theme_api::{DecorationBuffer, DecorationLayout, Point, Rect, ResizeEdge, Size};

use x11rb::connection::{Connection, RequestConnection};
use x11rb::errors::{ConnectError, ConnectionError, ReplyError, ReplyOrIdError};
use x11rb::protocol::randr::{self, ConnectionExt as _};
use x11rb::protocol::xproto::*;
use x11rb::protocol::{ErrorKind, Event};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, CURRENT_TIME, NONE};


/// A client's own top-level window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct XWindow(pub Window);

/// A decoration frame window (the reparenting target for a client).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct XFrame(pub Window);

#[derive(Debug, thiserror::Error)]
pub enum X11BackendError {
    #[error("failed to connect to the X server: {0}")]
    Connect(#[from] ConnectError),
    #[error("X11 connection error: {0}")]
    Connection(#[from] ConnectionError),
    #[error("X11 request failed: {0}")]
    Reply(#[from] ReplyError),
    #[error("X11 id allocation failed: {0}")]
    ReplyOrId(#[from] ReplyOrIdError),
    #[error("another window manager is already running on this display")]
    AnotherWmRunning,
}

/// X11 backend: implements `wm_core::Backend` via `x11rb`'s pure-Rust
/// connection, plus a handful of inherent methods (shell-window
/// creation, background painting, root/dock click routing) that exist
/// outside the `Backend` trait because they're desktop-shell concerns,
/// not window-manager-core concerns — `wm-core` has no notion of a dock
/// or a root menu.
pub struct X11Backend {
    conn: RustConnection,
    root: Window,
    screen_width: u16,
    screen_height: u16,
    /// The RandR version the server and this client agreed on at connect
    /// time, or `None` when the extension isn't there at all. Decides
    /// which enumeration path `query_monitors` takes, once, instead of
    /// probing on every call.
    randr: Option<(u32, u32)>,
    /// The monitor layout last read from RandR. Cached rather than
    /// queried per call for two reasons. Cost: `wm-core` asks
    /// `monitors()` on every placement decision, and a query is a round
    /// trip plus one more per monitor to resolve its name. Stability:
    /// `set_workareas` hands the core one rect per monitor *positionally*,
    /// so the list backing those indices must only ever change when the
    /// hardware layout does — at which point the shell is told to
    /// recompute (see `pending_screen_resize`).
    monitors: Vec<MonitorInfo>,
    depth: u8,
    image_byte_order: ImageOrder,
    gc: Gcontext,
    argb: Option<Argb>,
    /// Server-side pixmap currently installed as the root window's
    /// background. Kept alive for as long as X11 may repaint from it.
    root_background_pixmap: Option<Pixmap>,

    wm_protocols: Atom,
    /// `_XROOTPMAP_ID` / `ESETROOT_PMAP_ID` — the freedesktop-era
    /// convention (Esetroot, feh, hsetroot) naming the pixmap holding
    /// the current wallpaper, which pseudo-transparent apps (urxvt
    /// `-tr` among them) read and composite behind their own content.
    /// Published by `paint_background`/`paint_background_image`, so
    /// terminal transparency works with zero compositor involvement.
    xrootpmap_id: Atom,
    esetroot_pmap_id: Atom,
    net_wm_window_opacity: Atom,
    /// `(WM_CLASS name, opacity percent)` rules applied to new frames —
    /// see `add_opacity_rule`.
    opacity_rules: Vec<(String, u8)>,
    wm_delete_window: Atom,
    wm_take_focus: Atom,
    net_wm_pid: Atom,
    /// EWMH's UTF-8 title property — the one many modern toolkits
    /// (GTK, and by extension Chromium/Electron) actually set, often
    /// *without* also setting the legacy `WM_NAME`. Reading only
    /// `WM_NAME` left those windows with no title at all (confirmed
    /// live: Microsoft Edge miniaturized to a bare "?" icon).
    net_wm_name: Atom,
    utf8_string: Atom,
    /// `_MOTIF_WM_HINTS` — the property a client sets to ask not to be
    /// decorated. Motif itself is long gone, but nothing ever replaced
    /// this: EWMH specifies no "don't decorate me" hint, so every
    /// toolkit that draws its own titlebar (GTK3/4, Chromium and every
    /// Electron app on top of it, LibreOffice's VCL) still says it
    /// here, and a WM that doesn't read it frames those windows over
    /// chrome they already drew — two titlebars, only one of which
    /// moves the window.
    motif_wm_hints: Atom,
    /// Every EWMH atom this backend publishes or reacts to — see
    /// `EwmhAtoms`.
    ewmh: EwmhAtoms,
    /// Whether `BackendEvent::ShutdownRequested` has already been
    /// emitted for a dead connection. `poll_for_event` keeps returning
    /// the same fatal error on every call once the display is gone, so
    /// without this latch the event loop would be handed an endless
    /// stream of shutdown requests (and, before shutdown existed at
    /// all, would spin at 100% CPU forever — two zombie WMs after a
    /// display restart, confirmed live).
    shutdown_emitted: bool,

    known_clients: HashSet<Window>,
    /// Frame XID -> client XID, populated by `create_decoration`.
    frame_to_client: HashMap<Window, Window>,
    sequences_to_ignore: BinaryHeap<Reverse<u16>>,
    /// Cached server-format pixels per painted window (frame or shell
    /// window), replayed on `Expose` without re-touching the theme
    /// engine or re-converting byte order.
    painted: HashMap<Window, (u16, u16, Vec<u8>)>,
    /// Button press/release events on windows we don't recognize as a
    /// client/frame (root, dock, menu popups...) — the desktop shell in
    /// `chonkstep` drains these separately from `poll_event`, since
    /// `wm-core` has no concept of them. The `bool` is `pressed`.
    pending_shell_clicks: VecDeque<(Window, Point, MouseButton, bool)>,
    /// Latest pointer position over a non-client window (dock, menu,
    /// icon tile) — overwritten rather than queued, since only the most
    /// recent position matters for hover-highlight purposes.
    pending_shell_motion: Option<(Window, Point)>,
    /// Wheel notches over a window we don't recognize as a
    /// client/frame — same audience and same drain cadence as
    /// `pending_shell_clicks`, in its own queue because a wheel is not
    /// a `MouseButton` (see `wm_core::ScrollDelta`). Queued, not
    /// collapsed like `pending_shell_motion`: each notch is a separate
    /// command from the user, so keeping only the newest would eat the
    /// first two of a three-notch spin.
    pending_shell_scrolls: VecDeque<(Window, Point, ScrollDelta)>,
    /// Set by an RandR `ScreenChangeNotify` (e.g. the user resizing the
    /// Xephyr window this WM is nested in) — drained by the desktop
    /// shell in `chonkstep`, which repaints the background and
    /// repositions the dock at the new size. Not surfaced through
    /// `Backend::poll_event`/`BackendEvent` since resizing the whole
    /// screen isn't a per-client `wm-core` concern.
    ///
    /// A *monitor layout* change (a display plugged, unplugged, or
    /// rearranged) is queued here too, carrying whatever the screen size
    /// is at that moment even when it didn't change — see the
    /// `RandrNotify` arm in `translate_event` for why this reuses the
    /// resize path instead of adding a second drain.
    pending_screen_resize: Option<Size>,
    /// Keycode -> every keysym bound to it (one per shift level),
    /// snapshotted once at connect time via `GetKeyboardMapping`. Used
    /// both ways: forward (translating an incoming `KeyPress`'s keycode
    /// back to the keysym `wm-core` actually reasons about) and reverse
    /// (`grab_key` needs the keycode for a keysym like `XK_Tab`). A
    /// live keyboard remap mid-session (`MappingNotify`) isn't handled —
    /// an acceptable gap for a first cut, not a correctness issue for
    /// the fixed default bindings this WM grabs today.
    keyboard_map: HashMap<u8, Vec<u32>>,
    /// The modifier mask bit NumLock is bound to (0 if it couldn't be
    /// found) — `grab_key` also grabs every combination of this bit and
    /// `LockMask` (CapsLock, always bit 1) alongside the base modifiers,
    /// so a binding still fires with either lock key toggled on. Every
    /// real WM does this; skipping it is the classic "my keybinding
    /// stopped working because NumLock is on" bug.
    numlock_mask: u16,
    /// Per-application decoration overrides — see
    /// `wm_core::DecorationRules`. Matched against `WM_CLASS`.
    decoration_rules: wm_core::DecorationRules,
    /// The modifier currently grabbed for the move/resize gesture on
    /// every managed client, so a change can ungrab exactly what it
    /// installed. `None` when the gesture is disabled.
    drag_gesture_modifier: Option<Modifiers>,
    cursors: Cursors,
    /// The cursor last set on each frame — `set_frame_cursor` checks
    /// this before issuing a `ChangeWindowAttributes` so a stationary or
    /// slowly-moving pointer within the same hitbox doesn't re-set an
    /// already-current cursor on every single `MotionNotify`.
    frame_cursor: HashMap<Window, Option<ResizeEdge>>,
    /// The active pointer grab held for an interactive drag, named by
    /// the `DragHandle` it was handed out under — `None` whenever the
    /// server has granted this WM no grab. Recorded rather than
    /// inferred, because the one thing `ungrab_pointer` must never do
    /// is release a grab belonging to a drag other than the one asking
    /// (see its doc comment).
    drag_grab: Option<DragHandle>,
    /// Source of those handles: one per *granted* grab, never reused,
    /// and never zero — `DragHandle(0)` is the answer a refused grab
    /// gives, and it has to be a value no live grab can ever wear.
    next_drag_grab: u64,
}

impl X11Backend {
    /// Reads a text property (`WM_NAME`-shaped: a single string value of
    /// the given `type_atom`) and decodes it as UTF-8 — lossily for
    /// legacy `STRING`-typed properties (technically Latin-1/COMPOUND_TEXT,
    /// but in practice almost always plain ASCII in the wild), exactly
    /// for `UTF8_STRING`-typed ones. `None` for a missing or empty value,
    /// never an empty string — callers treat both as "no title."
    fn get_text_property(&self, window: Window, atom: Atom, type_atom: Atom) -> Option<String> {
        let cookie = self.conn.get_property(false, window, atom, type_atom, 0, u32::MAX).ok()?;
        let reply = cookie.reply().ok()?;
        if reply.value.is_empty() {
            None
        } else {
            Some(String::from_utf8_lossy(&reply.value).into_owned())
        }
    }

    /// Selects `PropertyChange` on a client's own window, so the two
    /// properties this WM keeps re-reading after the first map —
    /// `WM_NAME`/`_NET_WM_NAME` for the titlebar text, and
    /// `_MOTIF_WM_HINTS` for whether the client draws its own chrome —
    /// actually generate the `PropertyNotify`s `translate_event` turns
    /// into `TitleChanged`/`ChromeChanged`. A client selects its own
    /// event mask for its own purposes, but `PropertyChangeMask` isn't
    /// exclusive, so asking for it here doesn't disturb whatever the
    /// client asked for.
    ///
    /// Done the moment the window becomes known — its `MapRequest`, or
    /// the startup scan for windows that predate this WM — rather than
    /// when it gets framed, which is where this used to live. A client
    /// that draws its own chrome is fully managed but never framed, so
    /// selecting inside `create_decoration` would leave exactly the
    /// windows whose `_MOTIF_WM_HINTS` matters most permanently
    /// unwatched: an app that turns its own titlebar off after mapping
    /// could never be un-framed, and one that turns it back on could
    /// never get its frame back.
    ///
    /// The cost of selecting that early is a handful of
    /// `PropertyNotify`s from windows this WM turns out not to manage
    /// at all (a dock, a tooltip), which `translate_event` reports and
    /// the core drops. A few events nobody wanted is the right side of
    /// this trade against missing the one that decides whether a window
    /// wears two titlebars.
    fn watch_client_properties(&self, window: Window) {
        let _ = self
            .conn
            .change_window_attributes(window, &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE));
    }

    /// Maps or unmaps a client's *own* window, suppressing the
    /// `UnmapNotify` the request generates. That event is
    /// indistinguishable from the one a client sends when it withdraws
    /// itself, so without recording the request's sequence number for
    /// `should_ignore` every WM-driven hide — shading, a workspace
    /// switch, miniaturizing a frameless window — would reach
    /// `wm-core` as "this client is gone" and the window would be
    /// dropped from tracking mid-session. The mapping direction needs
    /// no suppression today (a `MapNotify` isn't translated at all),
    /// but is recorded the same way so the pairing can't rot.
    fn set_client_window_mapped(&mut self, window: Window, mapped: bool) {
        let seqno = if mapped {
            self.conn.map_window(window).ok().map(|c| c.sequence_number() as u16)
        } else {
            self.conn.unmap_window(window).ok().map(|c| c.sequence_number() as u16)
        };
        if let Some(seqno) = seqno {
            self.record_ignored_sequence(seqno);
        }
        let _ = self.conn.flush();
    }

    /// Connects to the X server and attempts to become the window
    /// manager (acquiring `SubstructureRedirect` on the root window).
    /// Fails loudly — via `X11BackendError::AnotherWmRunning` — if
    /// another WM already owns that.
    pub fn connect_and_become_wm(display: Option<&str>, scale: f32) -> Result<Self, X11BackendError> {
        let (conn, screen_num) = RustConnection::connect(display)?;
        let screen = conn.setup().roots[screen_num].clone();
        let root = screen.root;

        let change = ChangeWindowAttributesAux::new().event_mask(
            EventMask::SUBSTRUCTURE_REDIRECT
                | EventMask::SUBSTRUCTURE_NOTIFY
                | EventMask::BUTTON_PRESS
                | EventMask::BUTTON_RELEASE
                | EventMask::POINTER_MOTION
                | EventMask::PROPERTY_CHANGE,
        );
        let cookie = conn.change_window_attributes(root, &change)?;
        match cookie.check() {
            Ok(()) => {}
            Err(ReplyError::X11Error(ref error)) if error.error_kind == ErrorKind::Access => {
                return Err(X11BackendError::AnotherWmRunning);
            }
            Err(e) => return Err(e.into()),
        }

        let wm_protocols = conn.intern_atom(false, b"WM_PROTOCOLS")?.reply()?.atom;
        let wm_delete_window = conn.intern_atom(false, b"WM_DELETE_WINDOW")?.reply()?.atom;
        let wm_take_focus = conn.intern_atom(false, b"WM_TAKE_FOCUS")?.reply()?.atom;
        let net_wm_pid = conn.intern_atom(false, b"_NET_WM_PID")?.reply()?.atom;
        let net_wm_name = conn.intern_atom(false, b"_NET_WM_NAME")?.reply()?.atom;
        let utf8_string = conn.intern_atom(false, b"UTF8_STRING")?.reply()?.atom;
        let motif_wm_hints = conn.intern_atom(false, b"_MOTIF_WM_HINTS")?.reply()?.atom;
        let xrootpmap_id = conn.intern_atom(false, b"_XROOTPMAP_ID")?.reply()?.atom;
        let esetroot_pmap_id = conn.intern_atom(false, b"ESETROOT_PMAP_ID")?.reply()?.atom;
        let net_wm_window_opacity = conn.intern_atom(false, b"_NET_WM_WINDOW_OPACITY")?.reply()?.atom;
        let ewmh = EwmhAtoms::intern(&conn)?;

        // EWMH supporting-WM-check handshake (EWMH "_NET_SUPPORTING_WM_CHECK"):
        // pagers/taskbars/tools decide whether an EWMH-compliant WM is
        // running by reading this property on the root, following it to
        // a WM-owned window, and checking that window points back at
        // itself — without it, many refuse to send the client messages
        // handled in `translate_event` at all. The window itself is a
        // never-mapped 1x1 InputOnly child of root whose only job is to
        // exist (and vanish with this connection, which is exactly how
        // a watcher detects the WM dying).
        let check_window = conn.generate_id()?;
        conn.create_window(
            0, // InputOnly windows must use depth 0 (CopyFromParent)
            check_window,
            root,
            -1,
            -1,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            0,
            &CreateWindowAux::new().override_redirect(1),
        )?;
        conn.change_property32(PropMode::REPLACE, root, ewmh.net_supporting_wm_check, AtomEnum::WINDOW, &[check_window])?;
        conn.change_property32(PropMode::REPLACE, check_window, ewmh.net_supporting_wm_check, AtomEnum::WINDOW, &[check_window])?;
        // The spec wants the WM's name on the check window, UTF-8 typed
        // — this is where `wmctrl -m` and friends get it from.
        conn.change_property8(PropMode::REPLACE, check_window, net_wm_name, utf8_string, b"chonkstep")?;
        // Advertise exactly the protocol surface this WM implements —
        // EWMH says clients must treat an atom missing from
        // `_NET_SUPPORTED` as unsupported, so listing more than is real
        // would invite client messages nobody handles.
        conn.change_property32(PropMode::REPLACE, root, ewmh.net_supported, AtomEnum::ATOM, &ewmh.supported(net_wm_name))?;

        let gc = conn.generate_id()?;
        conn.create_gc(gc, root, &CreateGCAux::new().graphics_exposures(0))?;

        // See `Argb`'s doc comment. A GC is tied to a drawable depth,
        // so the 32-bit one is created against a throwaway 32-bit
        // pixmap.
        let argb = screen
            .allowed_depths
            .iter()
            .find(|d| d.depth == 32)
            .and_then(|d| d.visuals.iter().find(|v| v.class == VisualClass::TRUE_COLOR))
            .map(|v| v.visual_id)
            .and_then(|visual| {
                let colormap = conn.generate_id().ok()?;
                conn.create_colormap(ColormapAlloc::NONE, colormap, root, visual).ok()?;
                let scratch = conn.generate_id().ok()?;
                conn.create_pixmap(32, scratch, root, 1, 1).ok()?;
                let gc32 = conn.generate_id().ok()?;
                conn.create_gc(gc32, scratch, &CreateGCAux::new().graphics_exposures(0)).ok()?;
                let _ = conn.free_pixmap(scratch);
                Some(Argb { visual, colormap, gc: gc32 })
            });
        if argb.is_none() {
            tracing::info!("no 32-bit TrueColor visual; frames stay at root depth (no client alpha passthrough)");
        }

        // Without an explicit root cursor, a bare/nested X server (Xephyr,
        // Xvfb) shows nothing at all — real desktops get a default cursor
        // from elsewhere, but a from-scratch WM has to set one itself.
        // Every other window inherits this via the parent chain, so
        // setting it once on root is enough for the whole desktop. Drawn
        // to match `scale` (see `create_scaled_cursor`) rather than
        // pulled from the X server's built-in "cursor" font, whose glyphs
        // are a fixed small bitmap size with no way to scale them —
        // on a high-density panel that read as a comically tiny pointer
        // next to chonkstep's own (correctly `CHONKSTEP_SCALE`-scaled)
        // chrome, while apps with their own modern Xcursor-theme pointer
        // looked fine, making the mismatch obvious.
        let cursors = Cursors::create(&conn, root, scale)?;
        conn.change_window_attributes(root, &ChangeWindowAttributesAux::new().cursor(cursors.default))?;

        // RandR, for everything screen-geometry: the monitor layout
        // `monitors()` reports, and the notifications that say it moved.
        // Screen-change notifications cover a resize of the *virtual
        // screen* (Xephyr's `-resizeable` flag does exactly this when the
        // user drags its edge); CRTC- and output-change notifications
        // cover a display being plugged, unplugged, or rearranged, which
        // can leave the virtual screen's own size untouched.
        //
        // Nothing here is allowed to be fatal. A server without RandR
        // (bare Xvfb, an ancient remote X) still has to give the user a
        // session — it just gets one screen-sized monitor and never
        // learns about changes — so failures log and carry on rather
        // than aborting startup. This used to be a `?`, which meant a
        // RandR-less server couldn't start the WM at all.
        let randr = conn
            .randr_query_version(1, 6)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| (reply.major_version, reply.minor_version));
        match randr {
            Some((major, minor)) => {
                // RRNotify (crtc/output) arrived in RandR 1.2; asking a
                // 1.1 server to select those bits earns a protocol error
                // rather than events, so only ask where they exist.
                let mut mask = randr::NotifyMask::SCREEN_CHANGE;
                if randr_at_least(randr, 1, 2) {
                    mask |= randr::NotifyMask::CRTC_CHANGE | randr::NotifyMask::OUTPUT_CHANGE;
                }
                if let Err(e) = conn.randr_select_input(root, mask) {
                    tracing::warn!(?e, "RandR event selection failed; screen and monitor changes will go unnoticed");
                }
                tracing::debug!(major, minor, "RandR available");
            }
            None => tracing::info!("no RandR extension; treating the screen as a single monitor"),
        }

        let image_byte_order = conn.setup().image_byte_order;

        let keyboard_map = fetch_keyboard_map(&conn)?;
        let numlock_mask = find_numlock_mask(&conn, &keyboard_map)?;

        // The X screen is the *union* of every monitor (root spans all of
        // them), which is why it stays the fallback geometry and never
        // the per-monitor one.
        let screen_size = Size::new(screen.width_in_pixels as u32, screen.height_in_pixels as u32);
        let monitors = query_monitors(&conn, root, randr, screen_size);
        tracing::info!(?monitors, "monitor layout");

        conn.flush()?;

        Ok(Self {
            conn,
            root,
            screen_width: screen.width_in_pixels,
            screen_height: screen.height_in_pixels,
            randr,
            monitors,
            depth: screen.root_depth,
            image_byte_order,
            gc,
            argb,
            root_background_pixmap: None,
            wm_protocols,
            wm_delete_window,
            wm_take_focus,
            xrootpmap_id,
            esetroot_pmap_id,
            net_wm_window_opacity,
            opacity_rules: Vec::new(),
            net_wm_pid,
            net_wm_name,
            utf8_string,
            motif_wm_hints,
            ewmh,
            shutdown_emitted: false,
            known_clients: HashSet::new(),
            frame_to_client: HashMap::new(),
            sequences_to_ignore: BinaryHeap::new(),
            painted: HashMap::new(),
            pending_shell_clicks: VecDeque::new(),
            pending_shell_motion: None,
            pending_shell_scrolls: VecDeque::new(),
            pending_screen_resize: None,
            keyboard_map,
            numlock_mask,
            decoration_rules: wm_core::DecorationRules::default(),
            drag_gesture_modifier: None,
            cursors,
            frame_cursor: HashMap::new(),
            drag_grab: None,
            next_drag_grab: 1,
        })
    }

    pub fn root(&self) -> Window {
        self.root
    }

    /// The X11 connection's underlying socket, for a caller to block on
    /// (via `poll`/`epoll`) until there's actually something to read —
    /// see `x11rb`'s own `event_loop_integration` module docs, which
    /// document this exact `conn.stream().as_raw_fd()` pattern as the
    /// supported way to integrate with an external event loop instead
    /// of polling on a fixed timer.
    pub fn connection_fd(&self) -> std::os::unix::io::RawFd {
        use std::os::unix::io::AsRawFd;
        self.conn.stream().as_raw_fd()
    }

    /// Drains a pending screen-geometry change: an RandR
    /// `ScreenChangeNotify` (the user resized the Xephyr window this WM
    /// is nested in) or a monitor layout change (a display plugged or
    /// unplugged). The desktop shell should repaint the background,
    /// reposition the dock on the primary monitor, and recompute one
    /// workarea per monitor when it sees one — the size reported here is
    /// the whole screen, `monitors()` is where the per-monitor truth is.
    pub fn take_screen_resize(&mut self) -> Option<Size> {
        self.pending_screen_resize.take()
    }

    /// Re-rasterizes the pointer cursors after the session's UI scale
    /// changed under it. They are the only pixels on the desktop the
    /// theme engine does not produce (see `create_scaled_cursor` for
    /// why this WM draws its own pointers at all), so nothing else in
    /// the live-restyle path touches them: without this, a rescale left
    /// the pointer at whatever size the session was *started* at while
    /// every other piece of chrome around it changed size, which is the
    /// precise mismatch that got the X core cursor font thrown out in
    /// the first place.
    ///
    /// Cheap when the scale did not actually move, because the shell
    /// applies a whole look at once and will happily hand back the
    /// scale it already had. Compared with `to_bits` rather than `==`:
    /// a NaN that reached here would compare unequal to itself and
    /// rebuild five cursors on every apply forever, the same
    /// non-reflexive-float trap `usable_scale` guards in `chonk-shell`'s
    /// dockapp host.
    ///
    /// Degrades rather than fails, like the rest of this backend: a
    /// server that will not give us a new set leaves the session
    /// running on the set it has — wrongly sized, entirely usable —
    /// which beats losing the desktop over a pointer.
    pub fn set_ui_scale(&mut self, scale: f32) {
        if self.cursors.scale.to_bits() == scale.to_bits() {
            return;
        }
        let cursors = match Cursors::create(&self.conn, self.root, scale) {
            Ok(cursors) => cursors,
            Err(e) => {
                tracing::warn!(?e, scale, "rebuilding the cursors at the new scale failed; keeping the ones the session already has");
                return;
            }
        };
        let superseded = std::mem::replace(&mut self.cursors, cursors);

        // Root first. Every window that was never given a cursor of its
        // own inherits this one through the parent chain, so this single
        // request is what re-points the whole desktop.
        if let Err(e) = self.conn.change_window_attributes(self.root, &ChangeWindowAttributesAux::new().cursor(self.cursors.default)) {
            tracing::warn!(?e, "re-setting the root cursor after a scale change failed");
        }

        // Then every frame that *was*, which is every frame the pointer
        // has ever been over. `set_frame_cursor` returns early when the
        // edge it is handed matches the one it last set, and a scale
        // change does not move any pointer: each frame's memoized edge
        // is still what it was (`None`, the plain arrow, for all but the
        // one under the pointer). So the memo would answer "already
        // current" for the rest of the session while the id it stands
        // for is one we are about to free, and those frames would keep
        // naming a dead cursor. Re-issuing each frame's *current* edge
        // here fixes them without invalidating the memo, which stays
        // exactly as true afterwards as it was before.
        for (&frame, &edge) in &self.frame_cursor {
            let cursor = self.cursors.for_edge(edge);
            if let Err(e) = self.conn.change_window_attributes(frame, &ChangeWindowAttributesAux::new().cursor(cursor)) {
                tracing::warn!(?e, frame, "re-setting a frame's cursor after a scale change failed");
            }
        }

        // Only now the old ids, and only because they are queued behind
        // the re-points above: the server executes one client's requests
        // in the order they were written, so by the time it reaches a
        // `FreeCursor` nothing names that cursor any more. Freeing at
        // all is the point — cursors are server-side resources with no
        // owner in this process, and a session whose scale is nudged a
        // few times an hour would otherwise strand five of them per
        // nudge for as long as it runs.
        for cursor in superseded.all() {
            let _ = self.conn.free_cursor(cursor);
        }
        let _ = self.conn.flush();
        tracing::debug!(scale, "pointer cursors redrawn at the new scale");
    }

    /// The X screen: the union of every monitor, since the root window
    /// spans all of them. Not any one monitor's size.
    pub fn screen_size(&self) -> Size {
        Size::new(self.screen_width as u32, self.screen_height as u32)
    }

    /// Re-reads the monitor layout after RandR reported a change, and
    /// says whether it actually differs from the one the core has been
    /// placing windows against. A single plug event fans out into
    /// several RandR notifications (one per CRTC, one per output), so
    /// the comparison — not the event count — is what decides whether
    /// the shell gets asked to recompute.
    fn refresh_monitors(&mut self) -> bool {
        let monitors = query_monitors(&self.conn, self.root, self.randr, self.screen_size());
        if monitors == self.monitors {
            return false;
        }
        tracing::info!(?monitors, "monitor layout changed");
        self.monitors = monitors;
        true
    }

    /// Sets the root window's background color (the server redraws it
    /// automatically on `Expose` from then on — no repaint bookkeeping
    /// needed) and clears it to take effect immediately.
    pub fn paint_background(&mut self, rgb: (u8, u8, u8)) -> Result<(), X11BackendError> {
        // Routed through the pixmap path (a 1x1 the server tiles) even
        // though a plain background_pixel would paint identically:
        // pseudo-transparent apps can only composite what
        // `_XROOTPMAP_ID` names, so a solid wallpaper must publish a
        // pixmap too or transparency silently breaks on it.
        let buffer = DecorationBuffer { width: 1, height: 1, pixels: vec![rgb.0, rgb.1, rgb.2, 255] };
        self.paint_background_image(&buffer)
    }

    /// Installs a screen-sized RGBA buffer as the root background. The
    /// pixels live in an X server pixmap so uncovered root regions are
    /// restored by the server without an application-side expose loop.
    pub fn paint_background_image(&mut self, buffer: &DecorationBuffer) -> Result<(), X11BackendError> {
        if buffer.width == 0 || buffer.height == 0 {
            return Ok(());
        }
        let pixmap = self.conn.generate_id()?;
        let width = buffer.width.min(u16::MAX as u32) as u16;
        let height = buffer.height.min(u16::MAX as u32) as u16;
        self.conn.create_pixmap(self.depth, pixmap, self.root, width, height)?;
        let data = to_server_bytes(buffer, self.image_byte_order);
        if let Err(error) = self.put_image_rows(pixmap, width, height, &data) {
            let _ = self.conn.free_pixmap(pixmap);
            return Err(error.into());
        }
        self.conn.change_window_attributes(
            self.root,
            &ChangeWindowAttributesAux::new().background_pixmap(pixmap),
        )?;
        // Advertise the wallpaper pixmap before freeing its
        // predecessor: apps re-fetch on the PropertyNotify these
        // changes raise, and updating in this order keeps the window
        // where a client could read an already-freed pixmap id as
        // small as the protocol allows.
        for atom in [self.xrootpmap_id, self.esetroot_pmap_id] {
            self.conn.change_property32(PropMode::REPLACE, self.root, atom, AtomEnum::PIXMAP, &[pixmap])?;
        }
        self.conn.clear_area(false, self.root, 0, 0, 0, 0)?;
        self.conn.flush()?;

        if let Some(old) = self.root_background_pixmap.replace(pixmap) {
            self.conn.free_pixmap(old)?;
        }
        Ok(())
    }

    /// Registers a whole-window translucency rule: any client whose
    /// `WM_CLASS` instance or class name equals `class` (compared
    /// case-insensitively) gets `_NET_WM_WINDOW_OPACITY` set on its
    /// frame, which the session compositor honors uniformly. This is
    /// deliberately compositor-side rather than client-side alpha:
    /// urxvt's own 32-bit-visual background path leaves stale
    /// framebuffer garbage in regions it fails to repaint on scroll
    /// and resize (confirmed live — rows flipping between glass,
    /// garbage, and fully transparent), while a frame opacity property
    /// is applied by the compositor to the finished window image and
    /// cannot be inconsistent within a window.
    pub fn add_opacity_rule(&mut self, class: &str, opacity_percent: u8) {
        self.opacity_rules.push((class.to_ascii_lowercase(), opacity_percent.clamp(1, 100)));
    }

    fn apply_opacity_rule(&mut self, window: Window, frame: Window) {
        if self.opacity_rules.is_empty() {
            return;
        }
        let Ok(reply) = self
            .conn
            .get_property(false, window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 64)
            .map(|c| c.reply())
        else {
            return;
        };
        let Ok(reply) = reply else { return };
        let value = reply.value;
        let names: Vec<String> = value
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).to_ascii_lowercase())
            .collect();
        for (class, percent) in &self.opacity_rules {
            if names.iter().any(|n| n == class) {
                let opacity = (*percent as u64 * 0xFFFF_FFFF / 100) as u32;
                let _ = self.conn.change_property32(
                    PropMode::REPLACE,
                    frame,
                    self.net_wm_window_opacity,
                    AtomEnum::CARDINAL,
                    &[opacity],
                );
                return;
            }
        }
    }

    /// Publishes EWMH `_NET_FRAME_EXTENTS` on a client's own window:
    /// how many pixels of WM-drawn chrome sit on each side of it, in
    /// the spec's `left, right, top, bottom` order (not the
    /// clockwise/CSS order — getting that wrong reads as a window
    /// mysteriously offset by a titlebar height). A client that reasons
    /// about its own on-screen footprint — restoring a saved window
    /// position, sizing itself to a fraction of the workarea — has no
    /// other way to learn how much bigger than its content the thing
    /// the user sees actually is, since the frame is a window it never
    /// gets told about.
    ///
    /// A frameless (client-drawn) window publishes four zeros rather
    /// than nothing: an absent property means "this WM never says", and
    /// a client that has to guess guesses the chrome it would get if it
    /// were framed. Saying zero is the whole point of the property for
    /// exactly those windows.
    ///
    /// On the client window, never the frame, for the same reason as
    /// `publish_net_state`: readers only ever learn client ids, from
    /// `_NET_CLIENT_LIST`.
    pub fn publish_frame_extents(&mut self, window: XWindow, left: u32, right: u32, top: u32, bottom: u32) {
        let _ = self.conn.change_property32(
            PropMode::REPLACE,
            window.0,
            self.ewmh.net_frame_extents,
            AtomEnum::CARDINAL,
            &[left, right, top, bottom],
        );
        let _ = self.conn.flush();
    }

    /// Creates an unmanaged shell window (dock panel, menu popup, ...) —
    /// not a client, never goes through `Backend::create_decoration`.
    pub fn create_shell_window(
        &mut self,
        geometry: Rect,
        background: (u8, u8, u8),
        override_redirect: bool,
    ) -> Result<Window, X11BackendError> {
        let win = self.conn.generate_id()?;
        let aux = CreateWindowAux::new()
            .event_mask(EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION | EventMask::EXPOSURE)
            .background_pixel(pixel_from_rgb(background))
            .override_redirect(u32::from(override_redirect));
        self.conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            win,
            self.root,
            geometry.pos.x as i16,
            geometry.pos.y as i16,
            geometry.size.w as u16,
            geometry.size.h as u16,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &aux,
        )?;
        Ok(win)
    }

    pub fn map_shell_window(&self, win: Window) -> Result<(), X11BackendError> {
        self.conn.map_window(win)?;
        self.conn.flush()?;
        Ok(())
    }

    pub fn unmap_shell_window(&self, win: Window) -> Result<(), X11BackendError> {
        self.conn.unmap_window(win)?;
        self.conn.flush()?;
        Ok(())
    }

    /// Destroys a shell window outright — use this (not `unmap`) for
    /// one-shot popups (menus, icon tiles) that get freshly recreated
    /// every time they're shown, so repeated show/hide cycles don't
    /// leak invisible-but-still-alive X11 windows.
    pub fn destroy_shell_window(&mut self, win: Window) -> Result<(), X11BackendError> {
        self.painted.remove(&win);
        self.conn.destroy_window(win)?;
        self.conn.flush()?;
        Ok(())
    }

    pub fn raise_shell_window(&self, win: Window) -> Result<(), X11BackendError> {
        self.conn
            .configure_window(win, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE))?;
        self.conn.flush()?;
        Ok(())
    }

    pub fn configure_shell_window(&self, win: Window, geometry: Rect) -> Result<(), X11BackendError> {
        let aux = ConfigureWindowAux::new()
            .x(geometry.pos.x)
            .y(geometry.pos.y)
            .width(geometry.size.w)
            .height(geometry.size.h);
        self.conn.configure_window(win, &aux)?;
        self.conn.flush()?;
        Ok(())
    }

    /// Paints a `DecorationBuffer` onto any window (frame or shell) and
    /// caches the converted bytes so `Expose` can replay them.
    pub fn blit(&mut self, win: Window, buffer: &DecorationBuffer) {
        if buffer.width == 0 || buffer.height == 0 {
            return;
        }
        let (w, h) = (buffer.width as u16, buffer.height as u16);
        let data = to_server_bytes(buffer, self.image_byte_order);
        if let Err(e) = self.put_image_rows(win, w, h, &data) {
            tracing::warn!(?e, window = win, "put_image failed");
        }
        let _ = self.conn.flush();
        self.painted.insert(win, (w, h, data));
    }

    /// Sends `data` (a `w`x`h` `ZPixmap` buffer, top row first) to
    /// `drawable` via one or more `PutImage` requests, splitting it into
    /// horizontal row bands that each fit under the connection's actual
    /// negotiated request-size limit. A single `PutImage` covering a
    /// whole screen- or dock-height-tall buffer routinely exceeds that
    /// limit — even with the `BIG-REQUESTS` extension enabled, real
    /// servers still cap it well below a full HD frame's worth of RGBA8 —
    /// and x11rb rejects the oversized request client-side rather than
    /// silently truncating it, so without chunking the whole image simply
    /// never reaches the server.
    fn put_image_rows(&mut self, drawable: Drawable, w: u16, h: u16, data: &[u8]) -> Result<(), ConnectionError> {
        let stride = w as usize * 4;
        if stride == 0 || h == 0 {
            return Ok(());
        }
        // Frames are 32-bit when ARGB is available (see `Argb`);
        // everything else (root background pixmap, shell windows)
        // stays at the root depth. PutImage requires the depth and the
        // GC to match the destination drawable.
        let (depth, gc) = match &self.argb {
            Some(argb) if self.frame_to_client.contains_key(&drawable) => (32, argb.gc),
            _ => (self.depth, self.gc),
        };
        // A little under the real limit: PutImage's own header/padding
        // eats a small, fixed slice of it, and lowballing costs nothing.
        const REQUEST_OVERHEAD_BYTES: usize = 64;
        let budget = self.conn.maximum_request_bytes().saturating_sub(REQUEST_OVERHEAD_BYTES);
        let rows_per_chunk = (budget / stride).max(1);
        for (chunk_index, chunk) in data.chunks(rows_per_chunk * stride).enumerate() {
            let y = (chunk_index * rows_per_chunk) as i16;
            let chunk_h = (chunk.len() / stride) as u16;
            self.conn.put_image(ImageFormat::Z_PIXMAP, drawable, gc, w, chunk_h, 0, y, 0, depth, chunk)?;
        }
        Ok(())
    }

    /// Drains a button press/release that landed on a window `wm-core`
    /// doesn't know about (root background, dock, a menu popup) — the
    /// desktop shell checks the window id itself to decide what it was.
    /// The `bool` is `pressed` (`false` = release).
    pub fn take_shell_click(&mut self) -> Option<(Window, Point, MouseButton, bool)> {
        self.pending_shell_clicks.pop_front()
    }

    /// Drains the latest pointer position over a non-client window
    /// (dock, menu, icon tile), if it moved since the last drain — the
    /// desktop shell uses this to drive hover-highlight rendering (e.g.
    /// a menu item lighting up under the pointer).
    pub fn take_shell_motion(&mut self) -> Option<(Window, Point)> {
        self.pending_shell_motion.take()
    }

    /// Drains one wheel notch over a non-client window (dock, root,
    /// menu popup), oldest first. See `Backend::take_shell_scroll` for
    /// what a caller may rely on.
    pub fn take_shell_scroll(&mut self) -> Option<(Window, Point, ScrollDelta)> {
        self.pending_shell_scrolls.pop_front()
    }

    /// Takes an active pointer grab on the root for the duration of an
    /// interactive drag — a titlebar move, an edge resize, a dock icon
    /// being dragged along its column — so that the motion steering it
    /// and the button release ending it reach this window manager
    /// wherever the pointer travels.
    ///
    /// Without one, a drag only survives while the pointer stays over a
    /// window this WM selected input on. That holds for a frame, and is
    /// exactly false for a window whose client drew its own titlebar:
    /// there is no frame, the client owns every pixel under the
    /// pointer, and the move it asks for by `_NET_WM_MOVERESIZE` would
    /// begin and never be told the button came up — the window trailing
    /// the cursor with nothing held down until some unrelated event
    /// happened to break the spell. The same hole opens on a framed
    /// window whenever its size hints stop the frame following the
    /// pointer and the pointer ends up over the client's own content.
    ///
    /// `owner_events: true`, because this backend routes pointer events
    /// by the window they were reported against (see `translate_button`
    /// and the shell drains above), and that routing is worth keeping
    /// intact under the grab: with it, events over our own frames, dock
    /// and menu popups continue to be reported against those windows
    /// and reach the core and the shell exactly as they do ungrabbed.
    /// `false` would force every event to report against the root, so
    /// even a plain framed titlebar drag — the case that works today —
    /// would stop being recognizable as a frame event.
    ///
    /// `GrabMode::ASYNC` for both pointer and keyboard: a drag needs
    /// the events, not the input queue. Synchronous mode would freeze
    /// event processing until each `AllowEvents`, which is a desktop
    /// that stops responding for as long as anyone holds a titlebar.
    ///
    /// The reply is waited for — one round trip, once per drag — since
    /// a grab that was *not* granted (another client already holds the
    /// pointer, most plausibly a client that sent `_NET_WM_MOVERESIZE`
    /// without dropping its own implicit button grab first) must not be
    /// reported as one: the returned `DragHandle(0)` matches no live
    /// grab, so the teardown that follows correctly releases nothing.
    pub fn grab_pointer_for_drag(&mut self) -> DragHandle {
        let event_mask = EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION;
        let granted = match self.conn.grab_pointer(
            true,
            self.root,
            event_mask,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            NONE,
            NONE,
            CURRENT_TIME,
        ) {
            Ok(cookie) => match cookie.reply() {
                Ok(reply) if reply.status == GrabStatus::SUCCESS => true,
                Ok(reply) => {
                    tracing::warn!(?reply.status, "pointer grab for a drag not granted");
                    false
                }
                Err(e) => {
                    tracing::warn!(?e, "grab_pointer reply error");
                    false
                }
            },
            Err(e) => {
                tracing::warn!(?e, "grab_pointer request failed");
                false
            }
        };
        if !granted {
            return DragHandle(0);
        }
        let handle = DragHandle(self.next_drag_grab);
        self.next_drag_grab += 1;
        self.drag_grab = Some(handle);
        tracing::debug!(?handle, "pointer grabbed for a drag");
        handle
    }

    /// Releases the drag grab `handle` names — and only that one.
    ///
    /// The identity test is the entire point of the handle. A pointer
    /// grab this WM leaks is not a cosmetic bug: it is a desktop where
    /// no client sees the pointer again until this process exits, which
    /// is far worse than the stuck drag the grab exists to prevent. The
    /// way one gets leaked is a second drag beginning before the first
    /// was torn down — a dock drag overlapping a titlebar drag, a
    /// `_NET_WM_MOVERESIZE` arriving mid-resize. The server replaces
    /// this client's grab with the newer one, so an unconditional
    /// ungrab from the older drag's late teardown would free the grab
    /// the *live* drag is relying on and hand back the very bug being
    /// fixed here. A stale or repeated handle therefore does nothing,
    /// and so does `DragHandle(0)` — a grab that was refused never had
    /// anything to give back, and some other drag may legitimately hold
    /// the grab it did not get.
    ///
    /// A grab still held when this process dies needs no unwinding: the
    /// server drops every grab a client owns when its connection closes,
    /// which is also the only thing standing between a panic mid-drag
    /// and a frozen session.
    pub fn ungrab_pointer(&mut self, handle: DragHandle) {
        if self.drag_grab != Some(handle) {
            tracing::debug!(?handle, held = ?self.drag_grab, "ignoring an ungrab for a grab we are not holding");
            return;
        }
        // Forgotten before the request is attempted, not after it is
        // seen to succeed: if the request fails there is nothing left to
        // retry against (a dead connection has already dropped the grab
        // server-side), while a slot left occupied would make every
        // later ungrab of this handle a no-op and every later drag look
        // like it is stealing someone else's grab.
        self.drag_grab = None;
        if let Err(e) = self.conn.ungrab_pointer(CURRENT_TIME) {
            tracing::warn!(?e, "ungrab_pointer failed");
        }
        let _ = self.conn.flush();
    }

    fn keysym_for_keycode(&self, keycode: u8) -> Option<u32> {
        self.keyboard_map.get(&keycode)?.first().copied()
    }

    fn keycode_for_keysym(&self, keysym: u32) -> Option<u8> {
        self.keyboard_map.iter().find(|(_, syms)| syms.contains(&keysym)).map(|(&kc, _)| kc)
    }

    fn record_ignored_sequence(&mut self, seqno: u16) {
        self.sequences_to_ignore.push(Reverse(seqno));
    }

    fn should_ignore(&mut self, event: &Event) -> bool {
        let Some(seqno) = event.wire_sequence_number() else {
            return false;
        };
        while let Some(&Reverse(to_ignore)) = self.sequences_to_ignore.peek() {
            if to_ignore.wrapping_sub(seqno) <= u16::MAX / 2 {
                return to_ignore == seqno;
            }
            self.sequences_to_ignore.pop();
        }
        false
    }

    #[allow(clippy::too_many_arguments)]
    fn translate_button(
        &mut self,
        event_window: Window,
        // The child of `event_window` the pointer is over (`NONE` when
        // there is none) — consulted only for an event a grab redirected
        // to the root, where it is the sole record of what was under the
        // pointer.
        child: Window,
        x: i16,
        y: i16,
        detail: u8,
        pressed: bool,
        time: u32,
        state: u16,
    ) -> Option<BackendEvent<XWindow, XFrame>> {
        let local = Point::new(x as i32, y as i32);

        // A wheel reaches us as a button, so it has to be split off
        // before anything treats `detail` as one. The WM itself has no
        // scroll gesture (no titlebar or frame behavior binds the
        // wheel), so the only audience is the shell-surface family.
        if let Some(delta) = wheel_scroll(detail) {
            // Press only. The server sends a release immediately after
            // each wheel press purely to keep the button state machine
            // consistent — it marks no second event — so honoring both
            // would double every notch.
            //
            // The window test mirrors the shell-click routing below:
            // anything we did create as a frame, or know as a client,
            // is not the shell's. Neither of those cases can actually
            // arrive today (frames bind no wheel behavior, and the
            // per-window passive grabs `grab_button_passive` installs
            // are for real buttons only, so the server never sends us
            // a client's wheel), which is exactly why the check is
            // here: it states the routing rule rather than relying on
            // an event that merely happens not to be selected.
            if pressed && !self.frame_to_client.contains_key(&event_window) && !self.known_clients.contains(&event_window) {
                self.pending_shell_scrolls.push_back((event_window, local, delta));
            }
            return None;
        }

        let button = match detail {
            1 => MouseButton::Left,
            2 => MouseButton::Middle,
            3 => MouseButton::Right,
            _ => return None,
        };
        let mods = modifiers_from_state(state);

        if self.frame_to_client.contains_key(&event_window) {
            return Some(BackendEvent::PointerButton {
                surface: SurfaceRef::Frame(XFrame(event_window)),
                local,
                button,
                pressed,
                time_ms: time,
                mods,
            });
        }
        if self.known_clients.contains(&event_window) {
            return Some(BackendEvent::PointerButton {
                surface: SurfaceRef::Client(XWindow(event_window)),
                local,
                button,
                pressed,
                time_ms: time,
                mods,
            });
        }
        // A button coming up while this WM holds a drag grab, over a
        // window it never selected input on — which is every client's
        // own content — is reported against the grab window (the root),
        // not against what the pointer is over, so neither test above
        // recognizes the release that ends the drag. That is precisely
        // the release a client-decorated window's move hangs on: it has
        // no frame to catch it and its client owns every pixel under
        // the pointer. The grab's `child` field names the root child
        // the pointer is actually over — a frame, or the client itself
        // for a window that has none — which is the same vocabulary
        // those tests speak, so re-running them against it recovers the
        // routing a frame would have provided for free.
        //
        // Presses are deliberately not re-routed: `local` here is
        // root-relative (the event was reported against the root), and
        // a press is the one thing the core hit-tests against frame
        // geometry. A release it isn't — the drag ends wherever the
        // button comes up, before any hit test.
        //
        // The release is queued for the shell below as well rather than
        // consumed here, because the shell holds a grab of its own for
        // a dock or launcher-strip drag through this very call, and its
        // drag trackers are driven by exactly this root-reported
        // release (see the shell-click drain in `chonkstep`'s event
        // loop). One physical release, two audiences, neither of which
        // can end the other's drag.
        if !pressed && event_window == self.root && self.drag_grab.is_some() {
            let dragged = if self.frame_to_client.contains_key(&child) {
                Some(SurfaceRef::Frame(XFrame(child)))
            } else if self.known_clients.contains(&child) {
                Some(SurfaceRef::Client(XWindow(child)))
            } else {
                None
            };
            if let Some(surface) = dragged {
                self.pending_shell_clicks.push_back((event_window, local, button, pressed));
                return Some(BackendEvent::PointerButton { surface, local, button, pressed, time_ms: time, mods });
            }
            // No `child` to name, and the drag still ended. This is not
            // a rare shape: the pointer can be over the root itself,
            // over a window this desktop does not manage, or over
            // nothing the routing recognizes because edge snapping just
            // pulled the frame out from under it. `PointerButton`
            // cannot carry any of those — it needs a `SurfaceRef` —
            // and a release that goes unreported leaves the window
            // glued to the cursor, which is the whole failure this
            // grab exists to prevent. `DragEnded` says the one thing
            // that is true without naming a surface.
            self.pending_shell_clicks.push_back((event_window, local, button, pressed));
            return Some(BackendEvent::DragEnded);
        }
        // Root, dock, menu popups, or anything else we didn't create as
        // a client frame: hand it to the desktop shell instead — both
        // press and release, so shell code can tell the two apart (e.g.
        // a root press opens the root menu, but a menu item only
        // commits on release, matching how every button in this theme
        // works: arm on press, commit on release-while-still-over).
        self.pending_shell_clicks.push_back((event_window, local, button, pressed));
        None
    }

    fn translate_event(&mut self, event: Event) -> Option<BackendEvent<XWindow, XFrame>> {
        match event {
            Event::MapRequest(e) => {
                self.known_clients.insert(e.window);
                // Before the core is told about the window, so the
                // answer `client_draws_own_chrome` is about to give it
                // is already under watch: a client that rewrites
                // `_MOTIF_WM_HINTS` in the same breath as mapping (GTK
                // does this on realize) must not have that write fall
                // into a gap between "we know the window" and "we
                // selected for its property changes".
                self.watch_client_properties(e.window);
                Some(BackendEvent::MapRequest(XWindow(e.window)))
            }
            Event::UnmapNotify(e) => {
                if self.known_clients.remove(&e.window) {
                    Some(BackendEvent::Unmapped(XWindow(e.window)))
                } else {
                    None
                }
            }
            Event::DestroyNotify(e) => {
                self.known_clients.remove(&e.window);
                self.frame_to_client.retain(|_, client| *client != e.window);
                Some(BackendEvent::Destroyed(XWindow(e.window)))
            }
            Event::ConfigureRequest(e) => {
                let requested = Rect {
                    pos: Point::new(e.x as i32, e.y as i32),
                    size: Size::new(e.width as u32, e.height as u32),
                };
                Some(BackendEvent::ConfigureRequest { window: XWindow(e.window), requested })
            }
            Event::ButtonPress(e) => self.translate_button(e.event, e.child, e.event_x, e.event_y, e.detail, true, e.time, u16::from(e.state)),
            Event::ButtonRelease(e) => self.translate_button(e.event, e.child, e.event_x, e.event_y, e.detail, false, e.time, u16::from(e.state)),
            Event::MotionNotify(e) => {
                let local = Point::new(e.event_x as i32, e.event_y as i32);
                let surface_local = if self.frame_to_client.contains_key(&e.event) {
                    Some((SurfaceRef::Frame(XFrame(e.event)), local))
                } else if self.known_clients.contains(&e.event) {
                    Some((SurfaceRef::Client(XWindow(e.event)), local))
                } else {
                    self.pending_shell_motion = Some((e.event, local));
                    None
                };
                Some(BackendEvent::PointerMotion { root: Point::new(e.root_x as i32, e.root_y as i32), surface_local })
            }
            Event::Expose(e) => {
                if e.count == 0 {
                    if let Some((w, h, data)) = self.painted.get(&e.window).cloned() {
                        let _ = self.put_image_rows(e.window, w, h, &data);
                        let _ = self.conn.flush();
                    }
                }
                None
            }
            Event::EnterNotify(e) => {
                if self.frame_to_client.contains_key(&e.event) {
                    Some(BackendEvent::PointerEnter { surface: SurfaceRef::Frame(XFrame(e.event)) })
                } else {
                    None
                }
            }
            Event::KeyPress(e) => {
                let keysym = self.keysym_for_keycode(e.detail)?;
                let modifiers = modifiers_from_state(u16::from(e.state));
                Some(BackendEvent::KeyPress(KeyCombo { keysym, modifiers }))
            }
            // Releases only ever arrive while a modal keyboard grab is
            // active (nothing passively grabs them) — exactly when the
            // Alt-Tab switcher needs the Alt release that commits it.
            Event::KeyRelease(e) => {
                let keysym = self.keysym_for_keycode(e.detail)?;
                let modifiers = modifiers_from_state(u16::from(e.state));
                Some(BackendEvent::KeyRelease(KeyCombo { keysym, modifiers }))
            }
            Event::PropertyNotify(e) => {
                if !self.known_clients.contains(&e.window) {
                    return None;
                }
                // Watch both the legacy and EWMH title properties — a
                // client that only ever sets `_NET_WM_NAME` (common for
                // GTK/Chromium-based apps) would otherwise never trigger
                // a title update after its first map.
                if e.atom == u32::from(AtomEnum::WM_NAME) || e.atom == self.net_wm_name {
                    return Some(BackendEvent::TitleChanged(XWindow(e.window)));
                }
                // `_MOTIF_WM_HINTS` is not a map-time-only declaration.
                // Real applications rewrite it on a window that is
                // already up: LibreOffice swaps between its own
                // titlebar and the WM's when the user changes that
                // preference, and Chromium/Electron apps toggle it
                // entering and leaving their own full-screen mode. The
                // event says only that the property changed — deleted
                // included, `PropertyNotify` reports that too — so the
                // core re-reads rather than being handed a value, and
                // a deletion correctly reads back as "decorate it".
                if e.atom == self.motif_wm_hints {
                    return Some(BackendEvent::ChromeChanged(XWindow(e.window)));
                }
                None
            }
            Event::RandrScreenChangeNotify(e) => {
                self.screen_width = e.width;
                self.screen_height = e.height;
                // Order matters: the refresh's fallback path (no RandR
                // monitors to enumerate) reports a screen-sized monitor,
                // so it has to run against the *new* screen size.
                self.refresh_monitors();
                self.pending_screen_resize = Some(Size::new(e.width as u32, e.height as u32));
                None
            }
            // A CRTC or output changed: a display plugged in, unplugged,
            // switched off, or moved. The virtual screen's own size need
            // not change with it — measured on the test VM, `xrandr --fb
            // 1920x1200 --output Virtual-1 --mode 1600x1200` shrinks the
            // monitor inside an unchanged screen, and the
            // `ScreenChangeNotify` that accompanies it reports the same
            // size as before — so a size-carrying signal says nothing
            // about what actually moved, and the layout comparison in
            // `refresh_monitors` is what decides.
            //
            // Deliberately routed into `pending_screen_resize` rather
            // than a second drain of its own: what a shell does with that
            // signal is "recompute everything derived from screen
            // geometry" — wallpaper, dock and Clip placement on the
            // primary monitor, one workarea per monitor — which is
            // precisely what a layout change needs, and a parallel
            // mechanism would be one more thing every event loop and
            // every future backend has to remember to poll. The payload
            // is the current screen size even when unchanged: the size
            // was never the message, "your geometry is stale" is.
            //
            // Only CRTC- and output-change notifications are selected
            // (see `connect_and_become_wm`), so every `RandrNotify` that
            // arrives here is a layout signal; `refresh_monitors`
            // swallows the ones that turn out to change nothing.
            Event::RandrNotify(_) => {
                if self.refresh_monitors() {
                    self.pending_screen_resize = Some(self.screen_size());
                }
                None
            }
            Event::ClientMessage(e) => self.translate_client_message(&e),
            _ => None,
        }
    }

    /// Translates the EWMH client messages advertised in
    /// `_NET_SUPPORTED` (sent by pagers, taskbars, and tools like
    /// `wmctrl`/`xdotool`) into their backend-agnostic `BackendEvent`
    /// counterparts. Anything else — including EWMH messages this WM
    /// doesn't implement — is silently dropped, exactly what the spec
    /// wants for unsupported messages.
    fn translate_client_message(&self, e: &ClientMessageEvent) -> Option<BackendEvent<XWindow, XFrame>> {
        if e.type_ == self.ewmh.net_active_window {
            return Some(BackendEvent::ActivateRequested(XWindow(e.window)));
        }
        if e.type_ == self.ewmh.net_close_window {
            return Some(BackendEvent::CloseRequested(XWindow(e.window)));
        }
        if e.type_ == self.ewmh.net_wm_moveresize {
            // EWMH `_NET_WM_MOVERESIZE`: a client asking the window
            // manager to take over an interactive drag it has decided
            // has begun — which is what a client that drew its own
            // titlebar sends the instant the user presses on it.
            //
            // `data.l[2]` is the direction: 0..=7 are the spec's eight
            // resize edges in the order below, 8 is MOVE and 9 its
            // keyboard variant. Honouring these is not a nicety: a
            // client-drawn titlebar and client-drawn grips are the only
            // handles such a window has, since this WM deliberately
            // draws no chrome for it. `wm-core` runs the drag from its
            // own record either way; the client only names the edge.
            // Anything else (the CANCEL and keyboard-size values) is
            // dropped, which is what the spec asks for on anything
            // unsupported.
            let direction = e.data.as_data32()[2];
            const MOVE: u32 = 8;
            const KEYBOARD_MOVE: u32 = 9;
            if direction == MOVE || direction == KEYBOARD_MOVE {
                return Some(BackendEvent::MoveRequest(XWindow(e.window)));
            }
            let edge = match direction {
                0 => Some(ResizeEdge::NorthWest),
                1 => Some(ResizeEdge::North),
                2 => Some(ResizeEdge::NorthEast),
                3 => Some(ResizeEdge::East),
                4 => Some(ResizeEdge::SouthEast),
                5 => Some(ResizeEdge::South),
                6 => Some(ResizeEdge::SouthWest),
                7 => Some(ResizeEdge::West),
                _ => None,
            };
            if let Some(edge) = edge {
                return Some(BackendEvent::ResizeRequest { window: XWindow(e.window), edge });
            }
            return None;
        }
        if e.type_ == self.ewmh.net_current_desktop {
            // EWMH "_NET_CURRENT_DESKTOP": data.l[0] is the desktop a
            // pager wants switched to. Refuse an untrusted index before
            // it reaches the event queue; the core repeats this guard as
            // the final defence for every other caller.
            let requested = e.data.as_data32()[0];
            let Some(desktop) = workspace_index_from_ewmh(requested) else {
                tracing::warn!(desktop = requested, maximum = MAX_WORKSPACES - 1, "ignoring out-of-range _NET_CURRENT_DESKTOP request");
                return None;
            };
            return Some(BackendEvent::DesktopSwitchRequested(desktop));
        }
        if e.type_ == self.ewmh.net_wm_desktop {
            // EWMH "_NET_WM_DESKTOP": data.l[0] is the desktop a pager
            // wants `e.window` moved to. The spec's special 0xFFFFFFFF
            // value means "on all desktops" — this WM has no sticky
            // windows, so that request is swallowed rather than
            // misread as a move to a (nonsensical) huge desktop index.
            let desktop = e.data.as_data32()[0];
            if desktop == 0xFFFF_FFFF {
                return None;
            }
            let Some(desktop) = workspace_index_from_ewmh(desktop) else {
                tracing::warn!(desktop, maximum = MAX_WORKSPACES - 1, "ignoring out-of-range _NET_WM_DESKTOP request");
                return None;
            };
            return Some(BackendEvent::WindowDesktopRequested { window: XWindow(e.window), desktop });
        }
        if e.type_ == self.ewmh.net_wm_state {
            // EWMH "_NET_WM_STATE" client message layout:
            // data.l[0] = action (0 remove / 1 add / 2 toggle),
            // data.l[1] and data.l[2] = up to two property atoms — two
            // because a plain "maximize" toggles horizontal and
            // vertical in one message.
            let data = e.data.as_data32();
            let action = match data[0] {
                0 => NetStateAction::Remove,
                1 => NetStateAction::Add,
                2 => NetStateAction::Toggle,
                // Not a defined action — a malformed message, not one
                // to guess at.
                _ => return None,
            };
            let to_net_state = |atom: u32| {
                if atom == self.ewmh.net_wm_state_fullscreen {
                    Some(NetState::Fullscreen)
                } else if atom == self.ewmh.net_wm_state_maximized_horz {
                    Some(NetState::MaximizedHorz)
                } else if atom == self.ewmh.net_wm_state_maximized_vert {
                    Some(NetState::MaximizedVert)
                } else {
                    // Unrecognized property atoms are skipped, not
                    // rejected — EWMH wants the rest of the message
                    // still honored (see `NetState`'s doc comment).
                    None
                }
            };
            let mut recognized = [data[1], data[2]].into_iter().filter_map(to_net_state);
            // If neither property is one this WM acts on, there's
            // nothing to request — swallow the whole message.
            let first = recognized.next()?;
            let second = recognized.next();
            return Some(BackendEvent::NetStateRequested { window: XWindow(e.window), action, first, second });
        }
        None
    }
}

/// Converts the untrusted CARDINAL carried by an EWMH desktop request
/// only when it names a workspace the core permits.
fn workspace_index_from_ewmh(desktop: u32) -> Option<usize> {
    let desktop = usize::try_from(desktop).ok()?;
    (desktop < MAX_WORKSPACES).then_some(desktop)
}

/// The EWMH atoms this backend implements, interned in one place at
/// connect time (same pattern as the ICCCM atoms on `X11Backend`
/// itself, just grouped — there are enough of them that flat fields
/// would drown the struct). `_NET_WM_NAME`/`UTF8_STRING` predate this
/// group (the title-reading path needed them first) and stay where they
/// were.
struct EwmhAtoms {
    net_supported: Atom,
    net_supporting_wm_check: Atom,
    net_active_window: Atom,
    net_wm_moveresize: Atom,
    net_client_list: Atom,
    net_close_window: Atom,
    net_wm_state: Atom,
    net_wm_state_fullscreen: Atom,
    net_wm_state_maximized_horz: Atom,
    net_wm_state_maximized_vert: Atom,
    net_wm_state_shaded: Atom,
    net_wm_state_hidden: Atom,
    net_wm_window_type: Atom,
    net_wm_window_type_normal: Atom,
    net_wm_window_type_dialog: Atom,
    net_wm_window_type_desktop: Atom,
    net_wm_window_type_dock: Atom,
    net_wm_window_type_toolbar: Atom,
    net_wm_window_type_menu: Atom,
    net_wm_window_type_utility: Atom,
    net_wm_window_type_splash: Atom,
    net_wm_window_type_dropdown_menu: Atom,
    net_wm_window_type_popup_menu: Atom,
    net_wm_window_type_tooltip: Atom,
    net_wm_window_type_notification: Atom,
    net_wm_window_type_combo: Atom,
    net_wm_window_type_dnd: Atom,
    net_number_of_desktops: Atom,
    net_current_desktop: Atom,
    net_wm_desktop: Atom,
    net_workarea: Atom,
    net_frame_extents: Atom,
}

impl EwmhAtoms {
    fn intern(conn: &RustConnection) -> Result<Self, X11BackendError> {
        Ok(Self {
            net_supported: conn.intern_atom(false, b"_NET_SUPPORTED")?.reply()?.atom,
            net_supporting_wm_check: conn.intern_atom(false, b"_NET_SUPPORTING_WM_CHECK")?.reply()?.atom,
            net_active_window: conn.intern_atom(false, b"_NET_ACTIVE_WINDOW")?.reply()?.atom,
            net_wm_moveresize: conn.intern_atom(false, b"_NET_WM_MOVERESIZE")?.reply()?.atom,
            net_client_list: conn.intern_atom(false, b"_NET_CLIENT_LIST")?.reply()?.atom,
            net_close_window: conn.intern_atom(false, b"_NET_CLOSE_WINDOW")?.reply()?.atom,
            net_wm_state: conn.intern_atom(false, b"_NET_WM_STATE")?.reply()?.atom,
            net_wm_state_fullscreen: conn.intern_atom(false, b"_NET_WM_STATE_FULLSCREEN")?.reply()?.atom,
            net_wm_state_maximized_horz: conn.intern_atom(false, b"_NET_WM_STATE_MAXIMIZED_HORZ")?.reply()?.atom,
            net_wm_state_maximized_vert: conn.intern_atom(false, b"_NET_WM_STATE_MAXIMIZED_VERT")?.reply()?.atom,
            net_wm_state_shaded: conn.intern_atom(false, b"_NET_WM_STATE_SHADED")?.reply()?.atom,
            net_wm_state_hidden: conn.intern_atom(false, b"_NET_WM_STATE_HIDDEN")?.reply()?.atom,
            net_wm_window_type: conn.intern_atom(false, b"_NET_WM_WINDOW_TYPE")?.reply()?.atom,
            net_wm_window_type_normal: conn.intern_atom(false, b"_NET_WM_WINDOW_TYPE_NORMAL")?.reply()?.atom,
            net_wm_window_type_dialog: conn.intern_atom(false, b"_NET_WM_WINDOW_TYPE_DIALOG")?.reply()?.atom,
            net_wm_window_type_desktop: conn.intern_atom(false, b"_NET_WM_WINDOW_TYPE_DESKTOP")?.reply()?.atom,
            net_wm_window_type_dock: conn.intern_atom(false, b"_NET_WM_WINDOW_TYPE_DOCK")?.reply()?.atom,
            net_wm_window_type_toolbar: conn.intern_atom(false, b"_NET_WM_WINDOW_TYPE_TOOLBAR")?.reply()?.atom,
            net_wm_window_type_menu: conn.intern_atom(false, b"_NET_WM_WINDOW_TYPE_MENU")?.reply()?.atom,
            net_wm_window_type_utility: conn.intern_atom(false, b"_NET_WM_WINDOW_TYPE_UTILITY")?.reply()?.atom,
            net_wm_window_type_splash: conn.intern_atom(false, b"_NET_WM_WINDOW_TYPE_SPLASH")?.reply()?.atom,
            net_wm_window_type_dropdown_menu: conn.intern_atom(false, b"_NET_WM_WINDOW_TYPE_DROPDOWN_MENU")?.reply()?.atom,
            net_wm_window_type_popup_menu: conn.intern_atom(false, b"_NET_WM_WINDOW_TYPE_POPUP_MENU")?.reply()?.atom,
            net_wm_window_type_tooltip: conn.intern_atom(false, b"_NET_WM_WINDOW_TYPE_TOOLTIP")?.reply()?.atom,
            net_wm_window_type_notification: conn.intern_atom(false, b"_NET_WM_WINDOW_TYPE_NOTIFICATION")?.reply()?.atom,
            net_wm_window_type_combo: conn.intern_atom(false, b"_NET_WM_WINDOW_TYPE_COMBO")?.reply()?.atom,
            net_wm_window_type_dnd: conn.intern_atom(false, b"_NET_WM_WINDOW_TYPE_DND")?.reply()?.atom,
            net_number_of_desktops: conn.intern_atom(false, b"_NET_NUMBER_OF_DESKTOPS")?.reply()?.atom,
            net_current_desktop: conn.intern_atom(false, b"_NET_CURRENT_DESKTOP")?.reply()?.atom,
            net_wm_desktop: conn.intern_atom(false, b"_NET_WM_DESKTOP")?.reply()?.atom,
            net_workarea: conn.intern_atom(false, b"_NET_WORKAREA")?.reply()?.atom,
            net_frame_extents: conn.intern_atom(false, b"_NET_FRAME_EXTENTS")?.reply()?.atom,
        })
    }

    /// The `_NET_SUPPORTED` payload: every EWMH atom this WM actually
    /// implements (all of the above, plus `_NET_WM_NAME`, which is
    /// interned elsewhere — see `X11Backend::net_wm_name`).
    fn supported(&self, net_wm_name: Atom) -> Vec<Atom> {
        vec![
            self.net_supported,
            self.net_supporting_wm_check,
            self.net_active_window,
            self.net_client_list,
            self.net_close_window,
            // Advertised because a client checks this list before it
            // will send one: a CSD toolkit that does not see
            // `_NET_WM_MOVERESIZE` here falls back to moving itself
            // with raw ConfigureRequests, or to not moving at all.
            self.net_wm_moveresize,
            self.net_wm_state,
            self.net_wm_state_fullscreen,
            self.net_wm_state_maximized_horz,
            self.net_wm_state_maximized_vert,
            self.net_wm_state_shaded,
            self.net_wm_state_hidden,
            self.net_wm_window_type,
            self.net_wm_window_type_normal,
            self.net_wm_window_type_dialog,
            self.net_wm_window_type_desktop,
            self.net_wm_window_type_dock,
            self.net_wm_window_type_toolbar,
            self.net_wm_window_type_menu,
            self.net_wm_window_type_utility,
            self.net_wm_window_type_splash,
            self.net_wm_window_type_dropdown_menu,
            self.net_wm_window_type_popup_menu,
            self.net_wm_window_type_tooltip,
            self.net_wm_window_type_notification,
            self.net_wm_window_type_combo,
            self.net_wm_window_type_dnd,
            self.net_number_of_desktops,
            self.net_current_desktop,
            self.net_wm_desktop,
            self.net_workarea,
            self.net_frame_extents,
            net_wm_name,
        ]
    }
}

/// Whether the RandR version agreed on at connect time is at least
/// `major.minor`. `RRQueryVersion` answers with the *lower* of what the
/// client asked for and what the server implements, so this is the
/// honest test for "that request exists on this connection".
fn randr_at_least(version: Option<(u32, u32)>, major: u32, minor: u32) -> bool {
    matches!(version, Some(have) if have >= (major, minor))
}

/// Reads the live monitor layout from RandR. Three tiers, best first:
///
/// * RandR 1.5 `GetMonitors` — the only one that reports *logical*
///   monitors, which is what a user (and a dock) thinks in: a mirrored
///   pair is one monitor, not two stacked copies; the primary flag is
///   already resolved; and monitors an admin carved out by hand with
///   `xrandr --setmonitor` (splitting one ultrawide panel in two) are
///   reported as the two screens the user actually arranges windows on.
/// * A CRTC walk over `GetScreenResourcesCurrent`, for pre-1.5 servers,
///   with primary derived from `GetOutputPrimary`.
/// * One screen-sized monitor, for a server with no RandR at all.
///
/// Every tier degrades into the next rather than failing: a login
/// session that refuses to start because it couldn't enumerate monitors
/// is strictly worse than one that treats the screen as a single
/// display, which is exactly what this backend did before RandR.
fn query_monitors(conn: &RustConnection, root: Window, randr: Option<(u32, u32)>, screen: Size) -> Vec<MonitorInfo> {
    if randr_at_least(randr, 1, 5) {
        match monitors_via_get_monitors(conn, root) {
            Some(monitors) if !monitors.is_empty() => return normalize_monitors(monitors),
            _ => tracing::debug!("RandR 1.5 GetMonitors reported nothing; falling back to a CRTC walk"),
        }
    }
    if randr.is_some() {
        match monitors_via_crtcs(conn, root) {
            Some(monitors) if !monitors.is_empty() => return normalize_monitors(monitors),
            _ => tracing::debug!("RandR reported no active CRTCs; falling back to one screen-sized monitor"),
        }
    }
    vec![MonitorInfo { geometry: Rect { pos: Point::new(0, 0), size: screen }, name: "screen-0".to_string(), primary: true }]
}

/// RandR 1.5 `GetMonitors`. `get_active = true` on purpose: a monitor
/// whose outputs are all switched off has no pixels to place a window
/// on, and must not take up an index in the layout the shell reserves
/// workareas by.
fn monitors_via_get_monitors(conn: &RustConnection, root: Window) -> Option<Vec<MonitorInfo>> {
    let reply = conn.randr_get_monitors(root, true).ok()?.reply().ok()?;
    // Every name atom is asked for before any answer is awaited: x11rb
    // writes a request when its cookie is made and only blocks when one
    // is awaited, so this costs one round trip for the whole set instead
    // of one per monitor.
    let names: Vec<_> = reply.monitors.iter().map(|monitor| conn.get_atom_name(monitor.name).ok()).collect();
    let mut monitors = Vec::with_capacity(reply.monitors.len());
    for (monitor, name) in reply.monitors.iter().zip(names) {
        if monitor.width == 0 || monitor.height == 0 {
            continue;
        }
        let name = name
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| String::from_utf8_lossy(&reply.name).into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| format!("monitor-{}", monitors.len()));
        monitors.push(MonitorInfo {
            geometry: monitor_rect(monitor.x, monitor.y, monitor.width, monitor.height),
            name,
            primary: monitor.primary,
        });
    }
    Some(monitors)
}

/// Pre-1.5 fallback: one monitor per enabled CRTC. `...ResourcesCurrent`
/// rather than `GetScreenResources` because the latter asks the server
/// to re-probe every output — a visible stall, and on some drivers a
/// mode reset — which is far too aggressive for something a window
/// placement can trigger.
fn monitors_via_crtcs(conn: &RustConnection, root: Window) -> Option<Vec<MonitorInfo>> {
    let resources = conn.randr_get_screen_resources_current(root).ok()?.reply().ok()?;
    let primary_output = conn
        .randr_get_output_primary(root)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .map(|reply| reply.output)
        .filter(|output| *output != NONE);
    let crtcs: Vec<_> = resources
        .crtcs
        .iter()
        .filter_map(|&crtc| conn.randr_get_crtc_info(crtc, resources.config_timestamp).ok())
        .collect();

    // `(geometry, primary, output to name it after)`.
    let mut kept: Vec<(Rect, bool, randr::Output)> = Vec::new();
    for cookie in crtcs {
        let Ok(info) = cookie.reply() else { continue };
        // A disabled CRTC reports success with a zero size and no
        // outputs; `InvalidConfigTime` means the layout changed under
        // this very walk, in which case the change notification that
        // caused it is still queued and will run the walk again.
        if info.status != randr::SetConfig::SUCCESS || info.width == 0 || info.height == 0 || info.outputs.is_empty() {
            continue;
        }
        let geometry = monitor_rect(info.x, info.y, info.width, info.height);
        let primary = primary_output.is_some_and(|output| info.outputs.contains(&output));
        // Mirrored outputs are distinct CRTCs painting the same screen
        // rectangle. RandR 1.5 folds those into one logical monitor;
        // this tier has to do it by hand, because the shell reserves a
        // workarea per index and a duplicate rectangle would strand one
        // of them behind a dock that isn't there.
        if let Some(existing) = kept.iter_mut().find(|(rect, _, _)| *rect == geometry) {
            existing.1 |= primary;
            continue;
        }
        kept.push((geometry, primary, info.outputs[0]));
    }

    // Names, in the same request-then-await shape as above.
    let names: Vec<_> = kept
        .iter()
        .map(|(_, _, output)| conn.randr_get_output_info(*output, resources.config_timestamp).ok())
        .collect();
    let monitors = kept
        .into_iter()
        .zip(names)
        .map(|((geometry, primary, output), name)| {
            let name = name
                .and_then(|cookie| cookie.reply().ok())
                .map(|reply| String::from_utf8_lossy(&reply.name).into_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| format!("output-{output}"));
            MonitorInfo { geometry, name, primary }
        })
        .collect();
    Some(monitors)
}

fn monitor_rect(x: i16, y: i16, width: u16, height: u16) -> Rect {
    Rect { pos: Point::new(x as i32, y as i32), size: Size::new(width as u32, height as u32) }
}

/// Puts a freshly read layout into the shape `wm-core` assumes: ordered
/// left to right, then top to bottom, with exactly one primary.
///
/// The order has to be a function of the *geometry*, never of RandR's
/// reply order, because `set_workareas` hands the core one rect per
/// monitor positionally — if this list reshuffled between two calls for
/// one unchanged physical layout, every monitor's reserved dock strip
/// would silently jump to a different screen. Two monitors sharing an
/// origin (a projector mirroring at a different resolution, or a hand-
/// defined monitor overlapping a real one) break the tie by name, so
/// even that case is deterministic rather than reply-order dependent.
///
/// Exactly one primary because the shell puts the dock, the Clip and the
/// launcher strip on it: none flagged (a CRTC walk on a server with no
/// primary output set) would leave the desktop with nowhere to put its
/// chrome, and two flagged would put it in two places.
fn normalize_monitors(mut monitors: Vec<MonitorInfo>) -> Vec<MonitorInfo> {
    monitors.sort_by(|a, b| (a.geometry.pos.x, a.geometry.pos.y, &a.name).cmp(&(b.geometry.pos.x, b.geometry.pos.y, &b.name)));
    let primary = monitors.iter().position(|monitor| monitor.primary).unwrap_or(0);
    for (index, monitor) in monitors.iter_mut().enumerate() {
        monitor.primary = index == primary;
    }
    monitors
}

/// Whether a `poll_for_event` failure means the connection is dead for
/// good (the display server exited or the socket closed — reported by
/// x11rb as `IoError`, typically `UnexpectedEof`) as opposed to a
/// transient/recoverable condition. Only fatal errors justify asking
/// the core to shut down; see `poll_event`.
fn is_fatal_connection_error(error: &ConnectionError) -> bool {
    matches!(error, ConnectionError::IoError(_))
}

/// Keysym for `Num_Lock`, per `<X11/keysymdef.h>` — looked up once at
/// connect time so `grab_key` can also grab with this bit set, letting
/// a binding still fire when NumLock happens to be on.
const XK_NUM_LOCK: u32 = 0xff7f;

/// Snapshots keycode -> every keysym bound to it (one per shift level;
/// index 0 is the unshifted/base keysym, which is all `wm-core` ever
/// reasons about — modifier state is tracked separately, not via a
/// distinct shifted keysym).
fn fetch_keyboard_map(conn: &RustConnection) -> Result<HashMap<u8, Vec<u32>>, X11BackendError> {
    let setup = conn.setup();
    let min_kc = setup.min_keycode;
    let count = setup.max_keycode.saturating_sub(min_kc).saturating_add(1);
    let reply = conn.get_keyboard_mapping(min_kc, count)?.reply()?;
    let per_kc = (reply.keysyms_per_keycode as usize).max(1);
    let mut map = HashMap::new();
    for (i, chunk) in reply.keysyms.chunks(per_kc).enumerate() {
        map.insert(min_kc.wrapping_add(i as u8), chunk.to_vec());
    }
    Ok(map)
}

/// Which modifier bit (`ShiftMask`=bit0 .. `Mod5Mask`=bit7) NumLock is
/// bound to, via `GetModifierMapping` — this varies by keyboard layout/
/// server config (commonly `Mod2`, never guaranteed), so it has to be
/// queried rather than assumed. `0` if NumLock couldn't be found at all
/// (grabbing just skips the NumLock-held variants in that case).
fn find_numlock_mask(conn: &RustConnection, keyboard_map: &HashMap<u8, Vec<u32>>) -> Result<u16, X11BackendError> {
    let Some((&numlock_keycode, _)) = keyboard_map.iter().find(|(_, syms)| syms.contains(&XK_NUM_LOCK)) else {
        return Ok(0);
    };
    let reply = conn.get_modifier_mapping()?.reply()?;
    let per_slot = (reply.keycodes.len() / 8).max(1);
    for (slot, chunk) in reply.keycodes.chunks(per_slot).enumerate() {
        if chunk.contains(&numlock_keycode) {
            return Ok(1u16 << slot);
        }
    }
    Ok(0)
}

fn modifiers_to_x11_mask(mods: Modifiers) -> ModMask {
    let mut mask = ModMask::from(0u16);
    if mods.contains(Modifiers::SHIFT) {
        mask |= ModMask::SHIFT;
    }
    if mods.contains(Modifiers::CONTROL) {
        mask |= ModMask::CONTROL;
    }
    if mods.contains(Modifiers::ALT) {
        mask |= ModMask::M1;
    }
    if mods.contains(Modifiers::SUPER) {
        mask |= ModMask::M4;
    }
    mask
}

/// Every lock-key combination a binding needs grabbing under to keep
/// firing regardless of CapsLock/NumLock state: none held, CapsLock
/// alone, NumLock alone (if found), and both together. Skipping this is
/// the classic "my keybinding randomly stops working" bug every real WM
/// avoids by grabbing all four (or two, without NumLock) variants.
fn lock_key_variants(numlock_mask: u16) -> Vec<ModMask> {
    let mut variants = vec![ModMask::from(0u16), ModMask::LOCK];
    if numlock_mask != 0 {
        let numlock = ModMask::from(numlock_mask);
        variants.push(numlock);
        variants.push(numlock | ModMask::LOCK);
    }
    variants
}

fn pixel_from_rgb((r, g, b): (u8, u8, u8)) -> u32 {
    (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
}

fn button_index(button: MouseButton) -> ButtonIndex {
    match button {
        MouseButton::Left => ButtonIndex::M1,
        MouseButton::Middle => ButtonIndex::M2,
        MouseButton::Right => ButtonIndex::M3,
    }
}

/// A classic arrow-pointer polygon (tip at the origin, matching the
/// XC_left_ptr silhouette closely enough to read as "the arrow cursor"),
/// in unscaled unit coordinates.
const CURSOR_ARROW: &[(f32, f32)] = &[(0.0, 0.0), (0.0, 16.0), (4.0, 12.0), (7.0, 19.0), (9.0, 18.0), (6.0, 11.0), (11.0, 11.0)];
/// A double-headed arrow (⇕), hotspot at its center — the shape resize
/// cursors are built from. Traced as one outline: apex of the top
/// triangle, down its right side, along the shaft, out to the bottom
/// triangle, back up the other side.
const CURSOR_RESIZE_ARROW: &[(f32, f32)] = &[
    (5.0, 0.0),
    (10.0, 6.0),
    (7.0, 6.0),
    (7.0, 14.0),
    (10.0, 14.0),
    (5.0, 20.0),
    (0.0, 14.0),
    (3.0, 14.0),
    (3.0, 6.0),
    (0.0, 6.0),
];
const CURSOR_RESIZE_ARROW_CENTER: (f32, f32) = (5.0, 10.0);

/// Every cursor `wm-x11` needs, pre-rendered once at connect time (not
/// lazily per-hover) so hovering a resize corner never stalls on cursor
/// creation. All four track `CHONKSTEP_SCALE` the same way — see
/// `create_scaled_cursor`'s doc comment for why that matters and why
/// this is hand-drawn rather than sourced from the X core cursor font
/// or an Xcursor theme.
/// Server-side resources for 32-bit ARGB frame windows — the piece
/// that makes a translucent client's alpha SURVIVE reparenting: with
/// composite redirection, a client composites against its parent
/// frame's buffer, so a 24-bit frame flattens the client's alpha
/// before the compositor ever sees it (translucent terminals rendered
/// over black, confirmed live). Frames created at depth 32 keep the
/// alpha channel intact all the way to the compositor; decoration
/// pixels themselves are opaque (alpha 255 straight from the theme).
/// `None` when the server offers no 32-bit TrueColor visual (bare
/// Xvfb, say) — frames then fall back to the root depth exactly as
/// before.
struct Argb {
    visual: Visualid,
    colormap: Colormap,
    gc: Gcontext,
}

struct Cursors {
    default: Cursor,
    resize_v: Cursor,
    resize_h: Cursor,
    resize_se: Cursor,
    resize_sw: Cursor,
    /// The scale these five were rasterized at, kept so `set_ui_scale`
    /// can tell a real scale change from the shell re-applying a look
    /// at the scale it already had — the pixmaps, halo width and
    /// hotspot are all baked in at creation, so nothing about a cursor
    /// itself can be inspected to answer that afterwards.
    scale: f32,
}

impl Cursors {
    fn create(conn: &RustConnection, root: Window, scale: f32) -> Result<Self, X11BackendError> {
        let hotspot = CURSOR_RESIZE_ARROW_CENTER;
        Ok(Self {
            default: create_scaled_cursor(conn, root, scale, CURSOR_ARROW, (0.0, 0.0))?,
            resize_v: create_scaled_cursor(conn, root, scale, CURSOR_RESIZE_ARROW, hotspot)?,
            // East/West: the same double-arrow turned 90° to horizontal.
            resize_h: create_scaled_cursor(conn, root, scale, &rotate_shape(CURSOR_RESIZE_ARROW, hotspot, 90.0_f32.to_radians()), hotspot)?,
            // SouthEast (shared with NorthWest — the cursor shows the
            // resize *axis*, so opposite corners use the same glyph,
            // exactly as on a Mac) and SouthWest (shared with
            // NorthEast). The signs look backwards and are not: with
            // this rotation convention and X11's y-down pixel grid,
            // +45° turns the vertical double-arrow onto the ↗↙
            // diagonal, not ↘. Discovered by rasterizing the same
            // shapes in software for the Wayland compositor's cursor
            // set, whose orientation test pins the corrected mapping —
            // the original signs shipped the wrong diagonal on every
            // corner and nobody noticed from the code alone.
            resize_se: create_scaled_cursor(conn, root, scale, &rotate_shape(CURSOR_RESIZE_ARROW, hotspot, -45.0_f32.to_radians()), hotspot)?,
            resize_sw: create_scaled_cursor(conn, root, scale, &rotate_shape(CURSOR_RESIZE_ARROW, hotspot, 45.0_f32.to_radians()), hotspot)?,
            scale,
        })
    }

    /// Every cursor id in the set, for the one caller that has to treat
    /// them as a group rather than by role: `set_ui_scale`, freeing a
    /// superseded set. Written as an exhaustive destructure so that a
    /// sixth cursor added to the struct fails to compile here instead
    /// of quietly leaking a server-side resource on every rescale.
    fn all(&self) -> [Cursor; 5] {
        let Self { default, resize_v, resize_h, resize_se, resize_sw, scale: _ } = *self;
        [default, resize_v, resize_h, resize_se, resize_sw]
    }

    fn for_edge(&self, edge: Option<ResizeEdge>) -> Cursor {
        match edge {
            None => self.default,
            Some(ResizeEdge::South | ResizeEdge::North) => self.resize_v,
            Some(ResizeEdge::East | ResizeEdge::West) => self.resize_h,
            Some(ResizeEdge::SouthEast | ResizeEdge::NorthWest) => self.resize_se,
            Some(ResizeEdge::SouthWest | ResizeEdge::NorthEast) => self.resize_sw,
        }
    }
}

fn rotate_shape(points: &[(f32, f32)], center: (f32, f32), angle_rad: f32) -> Vec<(f32, f32)> {
    let (cx, cy) = center;
    let (sin, cos) = angle_rad.sin_cos();
    points
        .iter()
        .map(|&(x, y)| {
            let (dx, dy) = (x - cx, y - cy);
            (dx * cos - dy * sin + cx, dx * sin + dy * cos + cy)
        })
        .collect()
}

/// Draws and installs a cursor sized to `scale`, the same factor
/// `Theme::scaled` uses for every other pixel dimension in this WM's own
/// chrome — the X server's built-in "cursor" font (the old
/// `create_glyph_cursor` mechanism this replaces) has no size parameter
/// at all, so it couldn't be made to track `CHONKSTEP_SCALE` no matter
/// what value was passed to it. Drawn with core `PolyFillRectangle`/
/// `FillPoly` requests rather than the RENDER extension: it's plain
/// black-on-white (no anti-aliasing or alpha needed to look right at
/// this theme's blocky, hard-edged aesthetic), so a 1-bit source+mask
/// pixmap pair is enough — no new x11rb extension feature required.
///
/// `points`/`hotspot` are in the shape's own unscaled coordinate space
/// (which may include negative values, e.g. after rotation) — scaled by
/// `scale` and then shifted so everything lands at non-negative pixmap
/// coordinates before drawing.
fn create_scaled_cursor(conn: &RustConnection, root: Window, scale: f32, points: &[(f32, f32)], hotspot: (f32, f32)) -> Result<Cursor, X11BackendError> {
    let s = scale.max(1.0);
    let scaled: Vec<(f32, f32)> = points.iter().map(|&(x, y)| (x * s, y * s)).collect();
    let scaled_hotspot = (hotspot.0 * s, hotspot.1 * s);

    let min_x = scaled.iter().map(|p| p.0).fold(f32::INFINITY, f32::min).min(scaled_hotspot.0);
    let min_y = scaled.iter().map(|p| p.1).fold(f32::INFINITY, f32::min).min(scaled_hotspot.1);
    let max_x = scaled.iter().map(|p| p.0).fold(f32::NEG_INFINITY, f32::max).max(scaled_hotspot.0);
    let max_y = scaled.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max).max(scaled_hotspot.1);

    // The mask is the shape's silhouette dilated by `halo` pixels in
    // every direction (drawn as several copies of the same polygon,
    // offset by one pixel at a time) — everywhere mask=1 but source=0
    // renders as the *background* color, giving the pointer a light
    // outline against any backdrop instead of a flat black shape that
    // can disappear against a dark window.
    let halo = (s.round() as i32).clamp(1, 3);
    let margin = halo + 1;
    let shift_x = margin as f32 - min_x;
    let shift_y = margin as f32 - min_y;
    let w = ((max_x - min_x).round() as i32 + margin * 2).max(1) as u16;
    let h = ((max_y - min_y).round() as i32 + margin * 2).max(1) as u16;
    let hotspot_x = (scaled_hotspot.0 + shift_x).round() as i32;
    let hotspot_y = (scaled_hotspot.1 + shift_y).round() as i32;

    let source = conn.generate_id()?;
    conn.create_pixmap(1, source, root, w, h)?;
    let mask = conn.generate_id()?;
    conn.create_pixmap(1, mask, root, w, h)?;

    let gc = conn.generate_id()?;
    conn.create_gc(gc, source, &CreateGCAux::new().foreground(0).background(0))?;
    let clear = Rectangle { x: 0, y: 0, width: w, height: h };
    conn.poly_fill_rectangle(source, gc, &[clear])?;
    conn.poly_fill_rectangle(mask, gc, &[clear])?;
    conn.change_gc(gc, &ChangeGCAux::new().foreground(1))?;

    let poly_at = |dx: i32, dy: i32| -> Vec<x11rb::protocol::xproto::Point> {
        scaled
            .iter()
            .map(|&(x, y)| x11rb::protocol::xproto::Point {
                x: (x + shift_x).round() as i16 + dx as i16,
                y: (y + shift_y).round() as i16 + dy as i16,
            })
            .collect()
    };
    for dx in -halo..=halo {
        for dy in -halo..=halo {
            conn.fill_poly(mask, gc, PolyShape::COMPLEX, CoordMode::ORIGIN, &poly_at(dx, dy))?;
        }
    }
    conn.fill_poly(source, gc, PolyShape::COMPLEX, CoordMode::ORIGIN, &poly_at(0, 0))?;

    let cursor = conn.generate_id()?;
    // Foreground (the shape itself) black, background (the halo) white —
    // matching the classic look the old glyph-font cursor had.
    conn.create_cursor(cursor, source, mask, 0, 0, 0, 0xFFFF, 0xFFFF, 0xFFFF, hotspot_x as u16, hotspot_y as u16)?;

    let _ = conn.free_gc(gc);
    let _ = conn.free_pixmap(source);
    let _ = conn.free_pixmap(mask);

    Ok(cursor)
}

/// Translates an X11 `KeyButMask` bitfield (a button/key event's `state`)
/// into the backend-agnostic `Modifiers` `wm-core` reasons about. `Mod1`/
/// `Mod4` are the conventional Alt/Super mappings on essentially every
/// modern X11 setup (sourced from `xmodmap`'s defaults), not a hardcoded
/// keycode — good enough without adding a full modifier-remapping query.
/// The `ScrollDelta` an X11 button number means, or `None` for a
/// button that is not the wheel.
///
/// X11's core protocol has no axis event: a wheel detent arrives as an
/// ordinary press/release pair on buttons 4-7. Nothing in the protocol
/// spec says those numbers mean "wheel" — it is the convention every
/// X input driver has emitted and every toolkit has consumed since
/// IMPS/2 mice arrived (xf86-input-libinput still maps its scroll axes
/// onto exactly these four buttons, and `xev` shows them as plain
/// ButtonPress). Treating them as a wheel is reading the de-facto
/// standard, not guessing at one.
///
/// 4/5 are up/down and 6/7 are left/right. The horizontal pair is
/// supported rather than dropped because dropping it would have been
/// the *asymmetric* choice: the Wayland side gets a horizontal axis
/// whether we want one or not, so ignoring 6/7 here would leave the
/// two backends disagreeing about what a tilt-wheel does, which is the
/// one thing `Backend` exists to prevent.
///
/// Pure, and free-standing, so the mapping is testable with no X
/// server — `translate_button`, its only caller, needs a live
/// connection and therefore cannot be.
fn wheel_scroll(detail: u8) -> Option<ScrollDelta> {
    match detail {
        4 => Some(ScrollDelta { up: 1, right: 0 }),
        5 => Some(ScrollDelta { up: -1, right: 0 }),
        6 => Some(ScrollDelta { up: 0, right: -1 }),
        7 => Some(ScrollDelta { up: 0, right: 1 }),
        _ => None,
    }
}

fn modifiers_from_state(state: u16) -> Modifiers {
    let mut mods = Modifiers::empty();
    if state & u16::from(KeyButMask::SHIFT) != 0 {
        mods |= Modifiers::SHIFT;
    }
    if state & u16::from(KeyButMask::CONTROL) != 0 {
        mods |= Modifiers::CONTROL;
    }
    if state & u16::from(KeyButMask::MOD1) != 0 {
        mods |= Modifiers::ALT;
    }
    if state & u16::from(KeyButMask::MOD4) != 0 {
        mods |= Modifiers::SUPER;
    }
    mods
}

/// RGBA8 -> the server's native 32bpp `ZPixmap` byte layout for a
/// standard TrueColor visual (red_mask/green_mask/blue_mask ==
/// 0xFF0000/0x00FF00/0x0000FF, the near-universal case on modern Linux
/// X servers). LSBFirst servers store the low byte of the 32-bit pixel
/// value first (blue), MSBFirst servers store the high byte first
/// (padding/alpha). This is the one place `wm-theme`'s RGBA8 output gets
/// translated into what `PutImage` actually expects on the wire.
fn to_server_bytes(buffer: &DecorationBuffer, order: ImageOrder) -> Vec<u8> {
    let msb_first = order == ImageOrder::MSB_FIRST;
    let mut out = Vec::with_capacity(buffer.pixels.len());
    for px in buffer.pixels.as_chunks::<4>().0 {
        let (r, g, b, a) = (px[0], px[1], px[2], px[3]);
        if msb_first {
            out.extend_from_slice(&[a, r, g, b]);
        } else {
            out.extend_from_slice(&[b, g, r, a]);
        }
    }
    out
}

/// The inverse of [`to_server_bytes`]: the server's native 32bpp
/// `ZPixmap` byte layout (from `GetImage`) back to RGBA8 — used for
/// capturing a live window's pixels rather than painting our own theme
/// output. The server's 4th byte is unused padding for a 24-bit-depth
/// TrueColor visual, not meaningful alpha (a real window's content is
/// opaque), so alpha is always forced to fully-opaque rather than
/// trusted from the wire. Returns `None` if `data`'s length doesn't
/// match `width * height * 4` exactly, rather than silently truncating
/// — a mismatched capture is worth dropping, not showing a corrupted
/// partial image.
fn from_server_bytes(data: &[u8], width: u32, height: u32, order: ImageOrder) -> Option<DecorationBuffer> {
    let expected_len = (width as usize).checked_mul(height as usize)?.checked_mul(4)?;
    if data.len() != expected_len {
        return None;
    }
    let msb_first = order == ImageOrder::MSB_FIRST;
    let mut pixels = Vec::with_capacity(data.len());
    for px in data.as_chunks::<4>().0 {
        let (r, g, b) = if msb_first { (px[1], px[2], px[3]) } else { (px[2], px[1], px[0]) };
        pixels.extend_from_slice(&[r, g, b, 0xFF]);
    }
    Some(DecorationBuffer { width, height, pixels })
}

impl X11Backend {
    /// The string a `[decorations]` rule matches this window on: its
    /// `WM_CLASS` instance if there is one, else its class.
    ///
    /// Instance first because it is the more specific half and the one
    /// a user reads off `xprop`, and because the rules match by prefix,
    /// so naming the class still catches every instance under it. The
    /// compositor's XWayland arm resolves the identity the same way.
    fn window_identity(&self, window: XWindow) -> Option<String> {
        let cookie = self.conn.get_property(false, window.0, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, u32::MAX).ok()?;
        let reply = cookie.reply().ok()?;
        let mut parts = reply.value.split(|&b| b == 0).map(|s| String::from_utf8_lossy(s).into_owned());
        let instance = parts.next().unwrap_or_default();
        if !instance.is_empty() {
            return Some(instance);
        }
        parts.next().filter(|class| !class.is_empty())
    }
}

impl Backend for X11Backend {
    type WindowId = XWindow;
    type FrameId = XFrame;
    type ShellId = Window;

    // The shell-surface family delegates to the inherent methods the
    // shell called directly before it went backend-generic — X11 shell
    // surfaces are plain override-redirect windows. Errors are logged
    // and swallowed here (the inherent methods return Results for the
    // binary's own startup paths): a failed shell paint must never
    // take the WM down.

    fn create_shell_surface(&mut self, geometry: Rect, background: (u8, u8, u8), above: bool) -> Option<Self::ShellId> {
        match self.create_shell_window(geometry, background, above) {
            Ok(win) => Some(win),
            Err(e) => {
                tracing::warn!(?e, "failed to create shell surface");
                None
            }
        }
    }

    fn map_shell_surface(&mut self, id: Self::ShellId) {
        let _ = self.map_shell_window(id);
    }

    fn unmap_shell_surface(&mut self, id: Self::ShellId) {
        let _ = self.unmap_shell_window(id);
    }

    fn destroy_shell_surface(&mut self, id: Self::ShellId) {
        let _ = self.destroy_shell_window(id);
    }

    fn raise_shell_surface(&mut self, id: Self::ShellId) {
        let _ = self.raise_shell_window(id);
    }

    fn configure_shell_surface(&mut self, id: Self::ShellId, geometry: Rect) {
        let _ = self.configure_shell_window(id, geometry);
    }

    fn paint_shell_surface(&mut self, id: Self::ShellId, buffer: &DecorationBuffer) {
        self.blit(id, buffer);
    }

    fn release_shell_buffer(&mut self, _id: Self::ShellId) {
        // X11 owns the unmapped window's storage; this backend retains no
        // client-side pixel buffer or imported texture after `blit` returns.
    }

    fn paint_root_color(&mut self, rgb: (u8, u8, u8)) {
        if let Err(error) = self.paint_background(rgb) {
            tracing::warn!(?error, "failed to paint the desktop background");
        }
    }

    fn paint_root_image(&mut self, buffer: &DecorationBuffer) {
        // Worth a line: a wallpaper that silently fails to paint leaves
        // the desktop on whatever was behind it, which reads as a
        // rendering bug rather than an X error.
        if let Err(error) = self.paint_background_image(buffer) {
            tracing::warn!(?error, "failed to paint the desktop wallpaper");
        }
    }

    fn take_shell_click(&mut self) -> Option<(Self::ShellId, Point, MouseButton, bool)> {
        X11Backend::take_shell_click(self)
    }

    fn take_shell_motion(&mut self) -> Option<(Self::ShellId, Point)> {
        X11Backend::take_shell_motion(self)
    }

    fn take_shell_scroll(&mut self) -> Option<(Self::ShellId, Point, ScrollDelta)> {
        X11Backend::take_shell_scroll(self)
    }

    fn take_screen_resize(&mut self) -> Option<Size> {
        X11Backend::take_screen_resize(self)
    }

    fn set_ui_scale(&mut self, scale: f32) {
        X11Backend::set_ui_scale(self, scale)
    }

    // Forwarded to the inherent method for the same reason as the two
    // above: without this the trait's no-op default would quietly win
    // at every generic call site, and the property would never be
    // published despite the code to publish it existing.
    fn publish_frame_extents(&mut self, window: Self::WindowId, left: u32, right: u32, top: u32, bottom: u32) {
        X11Backend::publish_frame_extents(self, window, left, right, top, bottom)
    }

    fn screen_size(&self) -> Size {
        X11Backend::screen_size(self)
    }

    fn scan_existing_windows(&mut self) -> Vec<Self::WindowId> {
        let tree = match self.conn.query_tree(self.root) {
            Ok(cookie) => match cookie.reply() {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(?e, "query_tree reply failed");
                    return Vec::new();
                }
            },
            Err(e) => {
                tracing::warn!(?e, "query_tree request failed");
                return Vec::new();
            }
        };

        let mut result = Vec::new();
        for win in tree.children {
            let Ok(Ok(attr)) = self.conn.get_window_attributes(win).map(|c| c.reply()) else {
                continue;
            };
            if !attr.override_redirect && attr.map_state == MapState::VIEWABLE {
                self.known_clients.insert(win);
                // These windows never send a `MapRequest` — they were
                // already up when this WM started — so this is their
                // only chance to be put under property watch.
                self.watch_client_properties(win);
                result.push(XWindow(win));
            }
        }
        result
    }

    /// The cached RandR layout (see the `monitors` field): read at
    /// connect time and re-read only when RandR says something moved, so
    /// two calls in the same event-loop iteration cannot disagree about
    /// which monitor index means which screen.
    fn monitors(&self) -> Vec<MonitorInfo> {
        self.monitors.clone()
    }

    fn monitors_ref(&self) -> &[MonitorInfo] {
        &self.monitors
    }

    fn poll_event(&mut self) -> Option<BackendEvent<Self::WindowId, Self::FrameId>> {
        loop {
            let event = match self.conn.poll_for_event() {
                Ok(Some(e)) => e,
                Ok(None) => return None,
                // An IO error (UnexpectedEof when the display server
                // goes away) is unrecoverable: the connection never
                // comes back, and every subsequent poll fails the same
                // way instantly. Warning and returning `None` here —
                // the old behavior — left the event loop spinning on a
                // dead fd forever (two zombie WMs burning CPU after a
                // display restart, confirmed live), so a fatal error
                // now asks the core to exit instead. Emitted exactly
                // once: the loop may keep polling while it winds down,
                // and it needs `None` (not an endless shutdown stream)
                // from then on.
                Err(e) if is_fatal_connection_error(&e) => {
                    if self.shutdown_emitted {
                        return None;
                    }
                    self.shutdown_emitted = true;
                    tracing::error!(?e, "X11 connection lost; shutting down");
                    return Some(BackendEvent::ShutdownRequested);
                }
                Err(e) => {
                    tracing::warn!(?e, "poll_for_event failed");
                    return None;
                }
            };
            if self.should_ignore(&event) {
                continue;
            }
            if let Some(be) = self.translate_event(event) {
                return Some(be);
            }
            // Not translated (Expose handled internally, or a window we
            // don't care about) — keep draining.
        }
    }

    fn window_title(&self, window: Self::WindowId) -> Option<String> {
        // EWMH's `_NET_WM_NAME` (always UTF-8) is the modern, authoritative
        // title property — many toolkits (GTK, and by extension Chromium/
        // Electron apps like Microsoft Edge) set only this, not the legacy
        // `WM_NAME`, so reading `WM_NAME` alone left such windows with no
        // title at all (confirmed live: Edge miniaturized to a bare "?").
        // Fall back to `WM_NAME` for older/simpler clients that only set
        // that one.
        if let Some(title) = self.get_text_property(window.0, self.net_wm_name, self.utf8_string) {
            return Some(title);
        }
        self.get_text_property(window.0, u32::from(AtomEnum::WM_NAME), u32::from(AtomEnum::STRING))
    }

    fn window_class(&self, window: Self::WindowId) -> Option<WmClass> {
        let cookie = self.conn.get_property(false, window.0, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, u32::MAX).ok()?;
        let reply = cookie.reply().ok()?;
        let mut parts = reply.value.split(|&b| b == 0).map(|s| String::from_utf8_lossy(s).into_owned());
        let instance = parts.next()?;
        let class = parts.next().unwrap_or_default();
        Some(WmClass { instance, class })
    }

    fn window_pid(&self, window: Self::WindowId) -> Option<u32> {
        let cookie = self.conn.get_property(false, window.0, self.net_wm_pid, AtomEnum::CARDINAL, 0, 1).ok()?;
        let reply = cookie.reply().ok()?;
        let pid = reply.value32()?.next();
        pid
    }

    fn size_hints(&self, window: Self::WindowId) -> SizeHints {
        let hints = x11rb::properties::WmSizeHints::get_normal_hints(&self.conn, window.0)
            .ok()
            .and_then(|c| c.reply().ok())
            .flatten()
            .unwrap_or_default();
        let to_size = |pair: (i32, i32)| Size::new(pair.0.max(0) as u32, pair.1.max(0) as u32);
        SizeHints {
            min_size: hints.min_size.map(to_size),
            max_size: hints.max_size.map(to_size),
            resize_increment: hints.size_increment.map(to_size),
        }
    }

    fn supports_protocol(&self, window: Self::WindowId, protocol: WmProtocol) -> bool {
        let target = match protocol {
            WmProtocol::DeleteWindow => self.wm_delete_window,
            WmProtocol::TakeFocus => self.wm_take_focus,
        };
        let Ok(cookie) = self.conn.get_property(false, window.0, self.wm_protocols, AtomEnum::ATOM, 0, u32::MAX) else {
            return false;
        };
        let Ok(reply) = cookie.reply() else {
            return false;
        };
        reply.value32().map(|it| it.into_iter().any(|a| a == target)).unwrap_or(false)
    }

    fn window_parent(&self, window: Self::WindowId) -> Option<Self::WindowId> {
        // `WM_TRANSIENT_FOR`: one window id, the dialog's parent. ICCCM
        // has said for forty years that a window manager should keep
        // one above the other; this desktop did not, and a modal dialog
        // buried behind the document it belongs to is a document whose
        // close button appears broken.
        let cookie = self
            .conn
            .get_property(false, window.0, AtomEnum::WM_TRANSIENT_FOR, AtomEnum::WINDOW, 0, 1)
            .ok()?;
        let reply = cookie.reply().ok()?;
        let parent = reply.value32()?.next()?;
        // A window transient for itself, or for something this window
        // manager does not manage (the root, a destroyed window), has
        // no parent as far as stacking is concerned.
        if parent == window.0 || parent == self.root || !self.known_clients.contains(&parent) {
            return None;
        }
        Some(XWindow(parent))
    }

    fn set_decoration_rules(&mut self, rules: wm_core::DecorationRules) {
        self.decoration_rules = rules;
    }

    fn client_draws_own_chrome(&self, window: Self::WindowId) -> bool {
        // A `[decorations]` rule outranks the property, in both
        // directions — the same override the compositor honors, on the
        // same strings, so a config file written for one session means
        // the same thing in the other.
        if let Some(force_server_side) = self.decoration_rules.decision_for(self.window_identity(window).as_deref()) {
            return !force_server_side;
        }

        // Read as `AnyPropertyType`, not `CARDINAL`: Motif types this
        // property with the property atom itself, and a type-filtered
        // `GetProperty` against the wrong type answers successfully
        // with an empty value rather than failing — which would quietly
        // report every client-decorated window in the wild as wanting a
        // frame, i.e. exactly the bug this is here to fix, with no
        // error anywhere to explain it.
        //
        // Five words asked for, three required: see
        // `wm_core::hints_say_client_decorates`, which is the one
        // reader all three legs of this desktop now share. They had
        // drifted — this one accepted the three-word short form and the
        // compositor's XWayland arm (through smithay) demanded five —
        // so the same window could be framed in one session and not the
        // other.
        let Ok(cookie) = self.conn.get_property(false, window.0, self.motif_wm_hints, AtomEnum::ANY, 0, 5) else {
            return false;
        };
        let Ok(reply) = cookie.reply() else {
            return false;
        };
        let Some(values) = reply.value32() else {
            return false;
        };
        // Every way of failing to get an answer — property absent,
        // unreadable, not 32-bit format, truncated before the
        // `decorations` word, flag not set — converges on "decorate it"
        // inside the shared reader, deliberately. An ordinary X11
        // client says nothing about Motif hints and has relied on being
        // decorated for forty years, so "we could not tell" must mean
        // "decorate it"; the only thing that may un-frame a window is
        // the client explicitly asking.
        wm_core::hints_say_client_decorates(&values.collect::<Vec<u32>>())
    }

    fn window_geometry(&self, window: Self::WindowId) -> Rect {
        let fallback = Rect { pos: Point::new(0, 0), size: Size::new(200, 150) };
        let Ok(cookie) = self.conn.get_geometry(window.0) else {
            return fallback;
        };
        match cookie.reply() {
            Ok(g) => Rect { pos: Point::new(g.x as i32, g.y as i32), size: Size::new(g.width as u32, g.height as u32) },
            Err(e) => {
                tracing::warn!(?e, "get_geometry failed");
                fallback
            }
        }
    }

    fn capture_window_image(&self, window: Self::WindowId, size: Size) -> Option<DecorationBuffer> {
        let width = size.w.clamp(1, u16::MAX as u32) as u16;
        let height = size.h.clamp(1, u16::MAX as u32) as u16;
        let reply = match self.conn.get_image(ImageFormat::Z_PIXMAP, window.0, 0, 0, width, height, u32::MAX) {
            Ok(cookie) => match cookie.reply() {
                Ok(reply) => reply,
                Err(e) => {
                    tracing::warn!(?e, ?window, "get_image reply failed — window likely not viewable");
                    return None;
                }
            },
            Err(e) => {
                tracing::warn!(?e, ?window, "get_image request failed");
                return None;
            }
        };
        from_server_bytes(&reply.data, width as u32, height as u32, self.image_byte_order)
    }

    fn create_decoration(&mut self, window: Self::WindowId, layout: &DecorationLayout) -> Self::FrameId {
        let frame = match self.conn.generate_id() {
            Ok(id) => id,
            Err(e) => {
                tracing::error!(?e, "generate_id for frame failed");
                return XFrame(window.0);
            }
        };

        let aux = CreateWindowAux::new()
            .event_mask(
                EventMask::EXPOSURE
                    | EventMask::SUBSTRUCTURE_NOTIFY
                    // Without this, a client's *own* post-reparent
                    // resize attempts on itself apply directly — no
                    // `ConfigureRequest`, nothing for `wm-core` to see
                    // or have any say over, since redirect only ever
                    // applied to root's children. With it, they route
                    // through `handle_configure_request` like any other
                    // configure attempt, which is what makes
                    // `ClientFlags::SIZE_LOCKED` (and just generally,
                    // the WM having a say over a managed client's size
                    // at all after the initial map) possible.
                    | EventMask::SUBSTRUCTURE_REDIRECT
                    | EventMask::BUTTON_PRESS
                    | EventMask::BUTTON_RELEASE
                    | EventMask::POINTER_MOTION
                    | EventMask::ENTER_WINDOW,
            )
            // Deliberately no `background_pixel` here: with one set, the
            // server auto-clears the frame to that flat color on every
            // resize (before our own themed repaint has a chance to
            // land) and fires an `Expose` — during a fast resize drag,
            // dozens of these land per second, each one flashing the
            // background color for a moment before the real content
            // reappears. That's the literal mechanism behind a resize
            // strobing between the (very dark) titlebar gradient and a
            // flat gray flash, confirmed live. Leaving it unset makes
            // the server preserve whatever pixels were already there
            // instead of clearing them — stale-but-plausible content for
            // one frame, not a jarring color flash — and any area that
            // genuinely needs repainting still gets an `Expose` we
            // already handle (see `Event::Expose` below, replaying the
            // cached buffer). `bit_gravity(NORTH_WEST)` compounds this:
            // it tells the server to keep existing content anchored at
            // the frame's top-left and shift it during a resize instead
            // of discarding it outright, which is what makes the
            // titlebar specifically (always top-left) stay visually
            // stable throughout the whole drag.
            .bit_gravity(Gravity::NORTH_WEST);

        // Depth-32 frames when the server has an ARGB visual (see
        // `Argb`'s doc comment) — a window with a non-inherited visual
        // must also supply its own colormap and border pixel or the
        // server answers BadMatch.
        let (depth, visual, aux) = match &self.argb {
            Some(argb) => (32, argb.visual, aux.colormap(argb.colormap).border_pixel(0)),
            None => (COPY_DEPTH_FROM_PARENT, 0, aux),
        };
        if let Err(e) = self.conn.create_window(
            depth,
            frame,
            self.root,
            0,
            0,
            layout.frame_size.w.max(1) as u16,
            layout.frame_size.h.max(1) as u16,
            0,
            WindowClass::INPUT_OUTPUT,
            visual,
            &aux,
        ) {
            tracing::error!(?e, "create_window for frame failed");
        }

        if let Err(e) = self.conn.grab_server() {
            tracing::warn!(?e, "grab_server failed");
        }
        let _ = self.conn.change_save_set(SetMode::INSERT, window.0);
        let seqno = match self.conn.reparent_window(
            window.0,
            frame,
            layout.client_offset.x as i16,
            layout.client_offset.y as i16,
        ) {
            Ok(cookie) => Some(cookie.sequence_number() as u16),
            Err(e) => {
                tracing::warn!(?e, "reparent_window failed");
                None
            }
        };
        if let Some(seqno) = seqno {
            self.record_ignored_sequence(seqno);
        }
        let _ = self.conn.map_window(window.0);
        let _ = self.conn.ungrab_server();
        let _ = self.conn.flush();

        self.frame_to_client.insert(frame, window.0);
        self.apply_opacity_rule(window.0, frame);

        // The chrome this frame adds, published the moment it exists
        // rather than waiting for the core's next relayout — a client
        // that reads `_NET_FRAME_EXTENTS` does it while working out
        // where to put itself, which is immediately after being mapped.
        // The client's own window is still exactly the size the layout
        // was computed from (reparenting doesn't resize it), so its
        // geometry is what turns a frame size and a content offset into
        // four per-side widths. Queried after the server ungrab above,
        // not inside it: this is the one round trip on the path, and
        // holding every other X client still for it would be a poor
        // trade for a property nothing reads synchronously.
        if let Some(content) = self.conn.get_geometry(window.0).ok().and_then(|c| c.reply().ok()) {
            let left = layout.client_offset.x.max(0) as u32;
            let top = layout.client_offset.y.max(0) as u32;
            let right = layout.frame_size.w.saturating_sub(left + content.width as u32);
            let bottom = layout.frame_size.h.saturating_sub(top + content.height as u32);
            self.publish_frame_extents(window, left, right, top, bottom);
        }
        XFrame(frame)
    }

    fn destroy_decoration(&mut self, frame: Self::FrameId) {
        self.frame_to_client.remove(&frame.0);
        self.painted.remove(&frame.0);
        self.frame_cursor.remove(&frame.0);
        let _ = self.conn.destroy_window(frame.0);
        let _ = self.conn.flush();
    }

    fn release_decoration(&mut self, window: Self::WindowId, frame: Self::FrameId) {
        // Where the client currently sits in root coordinates. This has
        // to be asked before anything moves, and it has to be asked of
        // the *client*: the frame's origin is the chrome's top-left,
        // and reparenting the client there would shift its content up
        // and left by the titlebar and border it is about to stop
        // having. Reparenting is a move, so the number decides whether
        // an application that turns its own titlebar on mid-session
        // stays put or visibly jumps.
        //
        // If the client is already gone the translation fails, and the
        // fallbacks get progressively worse (the frame's origin: off by
        // the chrome; the screen corner: wrong but on screen). What
        // none of them do is skip the reparent, because the frame is
        // destroyed below either way and a client still parented to it
        // when that happens is destroyed with it.
        let position = self
            .conn
            .translate_coordinates(window.0, self.root, 0, 0)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| (reply.dst_x, reply.dst_y))
            .or_else(|| {
                self.conn
                    .get_geometry(frame.0)
                    .ok()
                    .and_then(|cookie| cookie.reply().ok())
                    .map(|geometry| (geometry.x, geometry.y))
            })
            .unwrap_or((0, 0));

        if let Err(e) = self.conn.grab_server() {
            tracing::warn!(?e, "grab_server failed");
        }
        // A `ReparentWindow` on a mapped window is three operations to
        // the server: it unmaps the window, moves it, and maps it again
        // — generating an `UnmapNotify`, a `ReparentNotify` and a
        // `MapNotify`, all carrying this request's sequence number.
        // That unmap is byte-for-byte the one a client sends when it
        // withdraws itself, so without the ignore `wm-core` would be
        // told the application had gone away at the exact moment it
        // asked to keep its window and draw its own chrome. All three
        // share one sequence number, so one recorded entry covers them,
        // and the automatic remap is why nothing here maps the client
        // back explicitly — doing so would also un-hide a window that
        // was deliberately unmapped (miniaturized, or on another
        // workspace) when its chrome changed.
        let seqno = match self.conn.reparent_window(window.0, self.root, position.0, position.1) {
            Ok(cookie) => Some(cookie.sequence_number() as u16),
            Err(e) => {
                tracing::warn!(?e, ?window, "reparent_window back to root failed");
                None
            }
        };
        if let Some(seqno) = seqno {
            self.record_ignored_sequence(seqno);
        }
        // Out of the save-set `create_decoration` put it in: that
        // entry exists so a client reparented *into* a frame is
        // rescued back to the root if this WM dies, and a client that
        // is already a child of the root has nothing to be rescued
        // from. `create_decoration` re-inserts if the window is framed
        // again later.
        let _ = self.conn.change_save_set(SetMode::DELETE, window.0);
        let _ = self.conn.ungrab_server();

        // Only now the frame, and only because the reparent is already
        // queued ahead of it: the server runs one client's requests in
        // the order they were written, so by the time `DestroyWindow`
        // executes the client is no longer a child of the frame and is
        // not destroyed with it. This is the whole reason
        // `release_decoration` exists rather than defaulting to
        // `destroy_decoration` on this backend.
        self.frame_to_client.remove(&frame.0);
        self.painted.remove(&frame.0);
        self.frame_cursor.remove(&frame.0);
        let _ = self.conn.destroy_window(frame.0);
        // No chrome around this window any more, said out loud — see
        // `publish_frame_extents` on why zeros rather than deleting the
        // property.
        self.publish_frame_extents(window, 0, 0, 0, 0);
        let _ = self.conn.flush();
    }

    fn paint_decoration(&mut self, frame: Self::FrameId, buffer: &DecorationBuffer) {
        self.blit(frame.0, buffer);
    }

    fn set_frame_cursor(&mut self, frame: Self::FrameId, edge: Option<ResizeEdge>) {
        if self.frame_cursor.get(&frame.0) == Some(&edge) {
            return;
        }
        let cursor = self.cursors.for_edge(edge);
        if let Err(e) = self.conn.change_window_attributes(frame.0, &ChangeWindowAttributesAux::new().cursor(cursor)) {
            tracing::warn!(?e, ?frame, "set_frame_cursor failed");
            return;
        }
        let _ = self.conn.flush();
        self.frame_cursor.insert(frame.0, edge);
    }

    fn set_frame_geometry(&mut self, frame: Self::FrameId, geometry: Rect) {
        let aux = ConfigureWindowAux::new()
            .x(geometry.pos.x)
            .y(geometry.pos.y)
            .width(geometry.size.w)
            .height(geometry.size.h);
        if let Err(e) = self.conn.configure_window(frame.0, &aux) {
            tracing::warn!(?e, "configure_window (frame) failed");
        }
        let _ = self.conn.flush();
    }

    fn resize_client(&mut self, window: Self::WindowId, size: Size) {
        let aux = ConfigureWindowAux::new().width(size.w).height(size.h);
        let _ = self.conn.configure_window(window.0, &aux);

        // ICCCM 4.1.5: a real `ConfigureWindow` already makes the
        // server generate a `ConfigureNotify`, but some clients (seen
        // in practice: alacritty not reliably resizing its own PTY/grid
        // to match a WM-driven resize on this system) apparently don't
        // pick that up reliably. Sending an explicit synthetic one —
        // the belt-and-suspenders move the ICCCM spec itself recommends
        // WMs make for exactly this kind of compatibility gap — costs
        // nothing and is a strict addition, never a regression.
        if let Ok(cookie) = self.conn.translate_coordinates(window.0, self.root, 0, 0) {
            if let Ok(pos) = cookie.reply() {
                let event = ConfigureNotifyEvent {
                    response_type: CONFIGURE_NOTIFY_EVENT,
                    sequence: 0,
                    event: window.0,
                    window: window.0,
                    above_sibling: NONE,
                    x: pos.dst_x,
                    y: pos.dst_y,
                    width: size.w as u16,
                    height: size.h as u16,
                    border_width: 0,
                    override_redirect: false,
                };
                let _ = self.conn.send_event(false, window.0, EventMask::STRUCTURE_NOTIFY, event);
            }
        }
        let _ = self.conn.flush();
    }

    fn configure_unmanaged(&mut self, window: Self::WindowId, geometry: Rect) {
        let aux = ConfigureWindowAux::new()
            .x(geometry.pos.x)
            .y(geometry.pos.y)
            .width(geometry.size.w)
            .height(geometry.size.h);
        let _ = self.conn.configure_window(window.0, &aux);
        let _ = self.conn.flush();
    }

    fn map_frame(&mut self, frame: Self::FrameId) {
        let _ = self.conn.map_window(frame.0);
        let _ = self.conn.flush();
    }

    fn unmap_frame(&mut self, frame: Self::FrameId) {
        let _ = self.conn.unmap_window(frame.0);
        let _ = self.conn.flush();
    }

    fn set_client_mapped(&mut self, window: Self::WindowId, mapped: bool) {
        // Shading hides the client's content while its frame stays up,
        // so this touches the client window only — and therefore has to
        // suppress its own `UnmapNotify`, or shading a window would be
        // indistinguishable from closing it (see
        // `set_client_window_mapped`).
        self.set_client_window_mapped(window.0, mapped);
    }

    fn map_frameless(&mut self, window: Self::WindowId) {
        // Identical to `map_unmanaged` today, and deliberately not
        // written as a call to it: that one shows a window this WM
        // decided *not* to manage (a dock, a tooltip) and is free to
        // grow policy of its own, while this one shows a fully managed
        // window that merely has no frame to map instead. A future edit
        // to either must not silently become an edit to both.
        self.set_client_window_mapped(window.0, true);
    }

    fn unmap_frameless(&mut self, window: Self::WindowId) {
        // The frameless counterpart of `unmap_frame`: with no frame to
        // hide, hiding the window means hiding the client itself, which
        // is precisely the case `set_client_window_mapped`'s
        // suppression exists for — an unsuppressed `UnmapNotify` here
        // would have the core forget a window that was only switched
        // away from, and it would never come back when the user
        // switched back.
        self.set_client_window_mapped(window.0, false);
    }

    // Asked of the server, because motion over a client's own window
    // never reaches this window manager and the remembered position is
    // therefore stale for exactly the windows that send
    // `_NET_WM_MOVERESIZE`. A failed query degrades to `None` and the
    // caller falls back, as it would for any backend that cannot answer.
    fn pointer_position(&self) -> Option<Point> {
        let reply = self.conn.query_pointer(self.root).ok()?.reply().ok()?;
        Some(Point::new(reply.root_x as i32, reply.root_y as i32))
    }

    fn raise(&mut self, frame: Self::FrameId) {
        let _ = self
            .conn
            .configure_window(frame.0, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
        let _ = self.conn.flush();
    }

    // The same request against the client window itself, for a managed
    // window that has no frame because its client drew its own chrome.
    // On X11 the two are the same operation on different windows: a
    // frameless client is parented to the root, so restacking it is
    // restacking exactly the thing on screen.
    fn raise_frameless(&mut self, window: Self::WindowId) {
        let _ = self
            .conn
            .configure_window(window.0, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
        let _ = self.conn.flush();
    }

    fn restack(&mut self, order_back_to_front: &[Self::FrameId]) {
        let mut prev: Option<Window> = None;
        for &frame in order_back_to_front {
            let aux = match prev {
                Some(p) => ConfigureWindowAux::new().sibling(p).stack_mode(StackMode::ABOVE),
                None => ConfigureWindowAux::new().stack_mode(StackMode::BELOW),
            };
            let _ = self.conn.configure_window(frame.0, &aux);
            prev = Some(frame.0);
        }
        let _ = self.conn.flush();
    }

    fn set_input_focus(&mut self, window: Self::WindowId) {
        let _ = self.conn.set_input_focus(InputFocus::PARENT, window.0, CURRENT_TIME);
        let _ = self.conn.flush();
    }

    fn send_close(&mut self, window: Self::WindowId) {
        if self.supports_protocol(window, WmProtocol::DeleteWindow) {
            let event = ClientMessageEvent::new(32, window.0, self.wm_protocols, [self.wm_delete_window, 0, 0, 0, 0]);
            let _ = self.conn.send_event(false, window.0, EventMask::NO_EVENT, event);
        } else {
            let _ = self.conn.kill_client(window.0);
        }
        let _ = self.conn.flush();
    }

    fn kill_client(&mut self, window: Self::WindowId) {
        let _ = self.conn.kill_client(window.0);
        let _ = self.conn.flush();
    }

    fn grab_pointer_for_drag(&mut self) -> DragHandle {
        X11Backend::grab_pointer_for_drag(self)
    }

    fn ungrab_pointer(&mut self, handle: DragHandle) {
        X11Backend::ungrab_pointer(self, handle)
    }

    fn position_client(&mut self, window: Self::WindowId, pos: Point) {
        let aux = ConfigureWindowAux::new().x(pos.x).y(pos.y);
        if let Err(e) = self.conn.configure_window(window.0, &aux) {
            tracing::warn!(?e, ?window, "position_client failed");
        }
        let _ = self.conn.flush();
    }

    fn refresh_client(&mut self, window: Self::WindowId, size: Size) {
        let event = ExposeEvent {
            response_type: EXPOSE_EVENT,
            sequence: 0,
            window: window.0,
            x: 0,
            y: 0,
            width: size.w.min(u16::MAX as u32) as u16,
            height: size.h.min(u16::MAX as u32) as u16,
            count: 0,
        };
        if let Err(e) = self.conn.send_event(false, window.0, EventMask::EXPOSURE, event) {
            tracing::warn!(?e, ?window, "refresh_client send_event failed");
        }
        let _ = self.conn.flush();
    }

    fn grab_keyboard(&mut self) {
        match self.conn.grab_keyboard(false, self.root, CURRENT_TIME, GrabMode::ASYNC, GrabMode::ASYNC) {
            Ok(cookie) => {
                if let Ok(reply) = cookie.reply() {
                    if reply.status != GrabStatus::SUCCESS {
                        tracing::warn!(?reply.status, "grab_keyboard not granted");
                    }
                }
            }
            Err(e) => tracing::warn!(?e, "grab_keyboard failed"),
        }
        let _ = self.conn.flush();
    }

    fn ungrab_keyboard(&mut self) {
        let _ = self.conn.ungrab_keyboard(CURRENT_TIME);
        let _ = self.conn.flush();
    }

    fn grab_key(&mut self, combo: KeyCombo) {
        let Some(keycode) = self.keycode_for_keysym(combo.keysym) else {
            tracing::warn!(keysym = combo.keysym, "no keycode found for keysym; cannot grab key");
            return;
        };
        let base = modifiers_to_x11_mask(combo.modifiers);
        for extra in lock_key_variants(self.numlock_mask) {
            if let Err(e) = self.conn.grab_key(true, self.root, base | extra, keycode, GrabMode::ASYNC, GrabMode::ASYNC) {
                tracing::warn!(?e, keysym = combo.keysym, "grab_key failed");
            }
        }
        let _ = self.conn.flush();
    }

    fn ungrab_key(&mut self, combo: KeyCombo) {
        let Some(keycode) = self.keycode_for_keysym(combo.keysym) else {
            return;
        };
        let base = modifiers_to_x11_mask(combo.modifiers);
        for extra in lock_key_variants(self.numlock_mask) {
            let _ = self.conn.ungrab_key(keycode, self.root, base | extra);
        }
        let _ = self.conn.flush();
    }

    fn grab_button_passive(&mut self, window: Self::WindowId, button: MouseButton) {
        // `owner_events: true` — while this passive grab is briefly
        // active for the triggering click, events still report against
        // the actual window under the pointer (this client) rather than
        // being forced to report against `window` regardless; combined
        // with `replay_pointer`, the client sees its own coordinates
        // normally once replayed. Async pointer/keyboard mode: don't
        // freeze the input stream while grabbed — an unfocused client's
        // *later* clicks (once no grab exists post-focus, or during the
        // brief window before replay) shouldn't stall waiting on us.
        // `ModMask::ANY` so the grab fires regardless of which
        // modifiers happen to be held (Shift-click to focus must still
        // focus).
        if let Err(e) = self.conn.grab_button(
            true,
            window.0,
            EventMask::BUTTON_PRESS,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            NONE,
            NONE,
            button_index(button),
            ModMask::ANY,
        ) {
            tracing::warn!(?e, ?window, "grab_button (passive) failed");
        }
        let _ = self.conn.flush();
    }

    fn set_drag_gesture_grab(&mut self, window: Self::WindowId, modifier: Option<Modifiers>) {
        // Distinct from the click-to-focus grab above in every way that
        // matters: that one is installed on *unfocused* clients only,
        // is removed the moment a window takes focus, and replays the
        // click onward. This one stays for the window's whole life,
        // because the window it has to work over is usually the focused
        // one, and its click is the window manager's alone — a client
        // that also received it would start a selection under a window
        // the user is only trying to move.
        //
        // Both buttons, both meanings: left drags, right resizes. That
        // is Window Maker's binding (which also accepts middle for a
        // move), and its grab is on the client window for exactly this
        // reason — a window with no titlebar has nothing else to grab.
        let previous = self.drag_gesture_modifier.take();
        for mods in previous.into_iter() {
            let base = modifiers_to_x11_mask(mods);
            for extra in lock_key_variants(self.numlock_mask) {
                let _ = self.conn.ungrab_button(ButtonIndex::M1, window.0, base | extra);
                let _ = self.conn.ungrab_button(ButtonIndex::M3, window.0, base | extra);
            }
        }
        if let Some(mods) = modifier {
            let base = modifiers_to_x11_mask(mods);
            // Every lock-key combination, or the gesture stops working
            // the moment Num Lock is on — the same trap the keybinding
            // grabs above already avoid, and the same fix.
            for extra in lock_key_variants(self.numlock_mask) {
                for button in [ButtonIndex::M1, ButtonIndex::M3] {
                    if let Err(e) = self.conn.grab_button(
                        // `owner_events: false`: this click is ours, and
                        // must not also be reported to the client.
                        false,
                        window.0,
                        EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION,
                        GrabMode::ASYNC,
                        GrabMode::ASYNC,
                        NONE,
                        NONE,
                        button,
                        base | extra,
                    ) {
                        tracing::warn!(?e, ?window, "grab_button for the modifier-drag failed");
                    }
                }
            }
        }
        self.drag_gesture_modifier = modifier;
        let _ = self.conn.flush();
    }

    fn ungrab_button_passive(&mut self, window: Self::WindowId, button: MouseButton) {
        if let Err(e) = self.conn.ungrab_button(button_index(button), window.0, ModMask::ANY) {
            tracing::warn!(?e, ?window, "ungrab_button failed");
        }
        let _ = self.conn.flush();
    }

    /// Lets the click that triggered a passive grab (see
    /// `grab_button_passive`) continue through to the client normally,
    /// once the WM has finished reacting to it (focusing/raising) —
    /// without this, a passively-grabbed client would never actually
    /// receive the click that focused it (e.g. to place a text cursor),
    /// only the WM would.
    fn replay_pointer(&mut self) {
        if let Err(e) = self.conn.allow_events(Allow::REPLAY_POINTER, CURRENT_TIME) {
            tracing::warn!(?e, "allow_events(ReplayPointer) failed");
        }
        let _ = self.conn.flush();
    }

    // -- EWMH ---------------------------------------------------------------

    fn window_type(&self, window: Self::WindowId) -> WindowType {
        // Up to 8 entries: `_NET_WM_WINDOW_TYPE` is a preference list
        // ("most preferred first"), and no real client declares more
        // types than that — 8 is comfortably past anything in the wild
        // without reading unbounded property data.
        let Ok(cookie) = self.conn.get_property(false, window.0, self.ewmh.net_wm_window_type, AtomEnum::ATOM, 0, 8) else {
            return WindowType::Normal;
        };
        let Ok(reply) = cookie.reply() else {
            return WindowType::Normal;
        };
        let Some(atoms) = reply.value32() else {
            return WindowType::Normal;
        };
        for atom in atoms {
            if atom == self.ewmh.net_wm_window_type_dialog {
                return WindowType::Dialog;
            }
            // Everything that draws its own chrome and positions
            // itself — the WM's only job for these is to stay out of
            // the way (see `WindowType::Unmanaged`).
            let unmanaged = [
                self.ewmh.net_wm_window_type_desktop,
                self.ewmh.net_wm_window_type_dock,
                self.ewmh.net_wm_window_type_toolbar,
                self.ewmh.net_wm_window_type_menu,
                self.ewmh.net_wm_window_type_splash,
                self.ewmh.net_wm_window_type_dropdown_menu,
                self.ewmh.net_wm_window_type_popup_menu,
                self.ewmh.net_wm_window_type_tooltip,
                self.ewmh.net_wm_window_type_notification,
                self.ewmh.net_wm_window_type_combo,
                self.ewmh.net_wm_window_type_dnd,
            ];
            if unmanaged.contains(&atom) {
                return WindowType::Unmanaged;
            }
            if atom == self.ewmh.net_wm_window_type_normal || atom == self.ewmh.net_wm_window_type_utility {
                return WindowType::Normal;
            }
            // An atom this WM doesn't recognize: keep scanning — the
            // list is ordered by client preference, and the spec wants
            // a WM to fall through to the first type it *does* know.
        }
        // No property, or nothing recognized in it — the spec's own
        // mandated fallback for a managed window.
        WindowType::Normal
    }

    fn map_unmanaged(&mut self, window: Self::WindowId) {
        let _ = self.conn.map_window(window.0);
        let _ = self.conn.flush();
    }

    fn publish_client_list(&mut self, clients: &[Self::WindowId]) {
        let ids: Vec<Window> = clients.iter().map(|w| w.0).collect();
        let _ = self.conn.change_property32(PropMode::REPLACE, self.root, self.ewmh.net_client_list, AtomEnum::WINDOW, &ids);
        let _ = self.conn.flush();
    }

    fn publish_active_window(&mut self, window: Option<Self::WindowId>) {
        // "No focused window" is published as window id 0 (`None` on
        // the wire), per the spec — not by deleting the property.
        let id = window.map_or(NONE, |w| w.0);
        let _ = self.conn.change_property32(PropMode::REPLACE, self.root, self.ewmh.net_active_window, AtomEnum::WINDOW, &[id]);
        let _ = self.conn.flush();
    }

    fn publish_workspaces(&mut self, count: usize, current: usize) {
        let _ = self.conn.change_property32(PropMode::REPLACE, self.root, self.ewmh.net_number_of_desktops, AtomEnum::CARDINAL, &[count as u32]);
        let _ = self.conn.change_property32(PropMode::REPLACE, self.root, self.ewmh.net_current_desktop, AtomEnum::CARDINAL, &[current as u32]);
        let _ = self.conn.flush();
    }

    fn publish_window_desktop(&mut self, window: Self::WindowId, desktop: usize) {
        // On the client's own window, not the frame, for the same
        // reason as `publish_net_state`: pagers look properties up by
        // the ids they got from `_NET_CLIENT_LIST`.
        let _ = self.conn.change_property32(PropMode::REPLACE, window.0, self.ewmh.net_wm_desktop, AtomEnum::CARDINAL, &[desktop as u32]);
        let _ = self.conn.flush();
    }

    fn publish_workarea(&mut self, area: Rect, workspace_count: usize) {
        // `_NET_WORKAREA` wants one x,y,w,h quadruple per desktop.
        // They're all identical here — the dock reserves the same strip
        // on every workspace (see the `Backend` trait's doc comment) —
        // but the property format still requires spelling each one out.
        let mut values = Vec::with_capacity(workspace_count * 4);
        for _ in 0..workspace_count {
            values.extend_from_slice(&[area.pos.x.max(0) as u32, area.pos.y.max(0) as u32, area.size.w, area.size.h]);
        }
        let _ = self.conn.change_property32(PropMode::REPLACE, self.root, self.ewmh.net_workarea, AtomEnum::CARDINAL, &values);
        let _ = self.conn.flush();
    }

    fn publish_net_state(&mut self, window: Self::WindowId, fullscreen: bool, max_h: bool, max_v: bool, shaded: bool, hidden: bool) {
        let mut atoms = Vec::with_capacity(5);
        if fullscreen {
            atoms.push(self.ewmh.net_wm_state_fullscreen);
        }
        if max_h {
            atoms.push(self.ewmh.net_wm_state_maximized_horz);
        }
        if max_v {
            atoms.push(self.ewmh.net_wm_state_maximized_vert);
        }
        if shaded {
            atoms.push(self.ewmh.net_wm_state_shaded);
        }
        if hidden {
            atoms.push(self.ewmh.net_wm_state_hidden);
        }
        // On the client's own window, not the frame: pagers/taskbars
        // look the state up by the client id they got from
        // `_NET_CLIENT_LIST`, and never learn frame ids at all. An
        // all-false state publishes an empty list rather than deleting
        // the property — same end state for readers, one less request
        // shape to reason about.
        let _ = self.conn.change_property32(PropMode::REPLACE, window.0, self.ewmh.net_wm_state, AtomEnum::ATOM, &atoms);
        let _ = self.conn.flush();
    }
}

/// Lets `wm-theme`'s reusable cascade-menu controller (and any other
/// stateful popup UI built the same way) drive this backend's shell
/// windows without depending on `wm-core::Backend` — popups aren't
/// clients, so they don't belong on that trait. Just thin delegation to
/// the inherent shell-window methods above; `grab_pointer`/
/// `ungrab_pointer` reuse the same inherent drag-grab pair
/// `Backend::grab_pointer_for_drag` forwards to rather than duplicating
/// the grab call — a popup's grab and a drag's grab are the same
/// server-side resource, so they must also share the bookkeeping that
/// keeps one from releasing the other's.
impl wm_theme_api::PopupHost for X11Backend {
    type PopupId = Window;

    fn create_popup(&mut self, geometry: Rect, background: (u8, u8, u8)) -> Option<Window> {
        let win = self.create_shell_window(geometry, background, true).ok()?;
        let _ = self.map_shell_window(win);
        let _ = self.raise_shell_window(win);
        Some(win)
    }

    fn destroy_popup(&mut self, popup: Window) {
        let _ = self.destroy_shell_window(popup);
    }

    fn paint_popup(&mut self, popup: Window, buffer: &DecorationBuffer) {
        self.blit(popup, buffer);
    }

    fn grab_pointer(&mut self) -> wm_theme_api::PopupGrab {
        let handle = X11Backend::grab_pointer_for_drag(self);
        wm_theme_api::PopupGrab(handle.0)
    }

    fn ungrab_pointer(&mut self, grab: wm_theme_api::PopupGrab) {
        X11Backend::ungrab_pointer(self, DragHandle(grab.0));
    }
}

/// Everything an X server is needed for is untestable here by
/// construction; what is testable is the pure translation this backend
/// does on top of it — the ordering and primary-flag contract
/// `wm-core` indexes its per-monitor workareas by
/// (`normalize_monitors`), and the wheel-button mapping the shell's
/// scroll channel is defined by (`wheel_scroll`).
#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(name: &str, x: i32, y: i32, w: u32, h: u32, primary: bool) -> MonitorInfo {
        MonitorInfo { geometry: Rect { pos: Point::new(x, y), size: Size::new(w, h) }, name: name.to_string(), primary }
    }

    #[test]
    fn ewmh_workspace_indices_stop_at_the_core_ceiling() {
        assert_eq!(workspace_index_from_ewmh(0), Some(0));
        assert_eq!(workspace_index_from_ewmh((MAX_WORKSPACES - 1) as u32), Some(MAX_WORKSPACES - 1));
        assert_eq!(workspace_index_from_ewmh(MAX_WORKSPACES as u32), None);
        assert_eq!(workspace_index_from_ewmh(u32::MAX), None);
    }

    /// The sign convention, spelled out against the physical gesture,
    /// because this is the exact spot where a backend can silently
    /// disagree with its sibling: `wm-wayland` derives the same
    /// `ScrollDelta` from `wl_pointer.axis`, whose vertical value is
    /// positive *downward* — the opposite of button 4. Equivalent
    /// physical input, equivalent event, is the whole contract, and it
    /// is one negation away from being false.
    #[test]
    fn the_wheel_buttons_map_to_the_gesture_they_name() {
        assert_eq!(wheel_scroll(4), Some(ScrollDelta { up: 1, right: 0 }), "button 4 is a roll away from the user");
        assert_eq!(wheel_scroll(5), Some(ScrollDelta { up: -1, right: 0 }), "button 5 is a roll toward the user");
        assert_eq!(wheel_scroll(6), Some(ScrollDelta { up: 0, right: -1 }), "button 6 is a tilt left");
        assert_eq!(wheel_scroll(7), Some(ScrollDelta { up: 0, right: 1 }), "button 7 is a tilt right");
    }

    /// Buttons 1-3 must stay clicks: they are the ones the shell (and
    /// the dock's middle-click reorder) already routes through
    /// `take_shell_click`, and a wheel branch that swallowed one would
    /// make it vanish from that path entirely rather than fail loudly.
    #[test]
    fn the_real_buttons_are_not_wheel_notches() {
        for detail in [1u8, 2, 3] {
            assert_eq!(wheel_scroll(detail), None, "button {detail} is a click");
        }
    }

    /// Buttons 8/9 are the thumb "back"/"forward" pair, and 0 is not a
    /// button at all. Neither is a wheel; both must fall through to
    /// `translate_button`'s existing "unknown button" drop rather than
    /// being invented into scroll steps.
    #[test]
    fn side_buttons_are_not_wheel_notches() {
        for detail in [0u8, 8, 9, 10, 255] {
            assert_eq!(wheel_scroll(detail), None, "button {detail} is not a wheel");
        }
    }

    /// Every notch a backend may report is non-zero — the drain's
    /// stated contract, which callers are allowed to lean on.
    #[test]
    fn no_wheel_button_produces_an_empty_delta() {
        for detail in 0u8..=255 {
            if let Some(delta) = wheel_scroll(detail) {
                assert!(!delta.is_zero(), "button {detail} queued a scroll that says nothing happened");
            }
        }
    }

    #[test]
    fn orders_left_to_right_then_top_to_bottom() {
        let ordered = normalize_monitors(vec![
            monitor("right", 1920, 0, 1920, 1080, false),
            monitor("below-left", 0, 1080, 1920, 1080, false),
            monitor("left", 0, 0, 1920, 1080, true),
        ]);
        let names: Vec<&str> = ordered.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["left", "below-left", "right"]);
    }

    /// The order must depend only on the geometry: RandR is free to
    /// report the same unchanged layout in a different order, and a
    /// reshuffle would silently move every monitor's reserved dock strip
    /// to a different screen.
    #[test]
    fn order_is_independent_of_reply_order() {
        let layout = || vec![monitor("b", 1920, 0, 1920, 1080, false), monitor("a", 0, 0, 1920, 1080, true), monitor("c", 3840, 0, 1280, 1024, false)];
        let mut reversed = layout();
        reversed.reverse();
        assert_eq!(normalize_monitors(layout()), normalize_monitors(reversed));
    }

    #[test]
    fn same_origin_monitors_tie_break_by_name() {
        let ordered = normalize_monitors(vec![monitor("projector", 0, 0, 1024, 768, false), monitor("panel", 0, 0, 1920, 1080, true)]);
        let names: Vec<&str> = ordered.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["panel", "projector"]);
    }

    #[test]
    fn keeps_exactly_one_primary_when_the_server_flags_several() {
        let normalized = normalize_monitors(vec![monitor("left", 0, 0, 1920, 1080, true), monitor("right", 1920, 0, 1920, 1080, true)]);
        assert_eq!(normalized.iter().filter(|m| m.primary).count(), 1);
        assert!(normalized[0].primary, "the leftmost of two flagged monitors keeps the flag");
    }

    /// A CRTC walk on a server with no primary output set reports none —
    /// the shell still needs somewhere to put the dock.
    #[test]
    fn promotes_the_first_monitor_when_none_is_flagged() {
        let normalized = normalize_monitors(vec![monitor("right", 1920, 0, 1920, 1080, false), monitor("left", 0, 0, 1920, 1080, false)]);
        assert!(normalized[0].primary);
        assert_eq!(normalized[0].name, "left");
        assert_eq!(normalized.iter().filter(|m| m.primary).count(), 1);
    }

    /// A monitor left of the origin is normal (`xrandr --left-of` on the
    /// primary), and negative coordinates must sort before zero rather
    /// than wrap through an unsigned cast somewhere.
    #[test]
    fn negative_origins_sort_before_the_primary() {
        let ordered = normalize_monitors(vec![monitor("primary", 0, 0, 1920, 1080, true), monitor("left-of", -1280, 0, 1280, 1024, false)]);
        let names: Vec<&str> = ordered.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["left-of", "primary"]);
        assert!(ordered[1].primary, "sorting must not move the primary flag off its monitor");
    }
}
