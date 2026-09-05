//! XWayland geometry and pointer coordinates against a real nested session.
//!
//! The compositor stores and renders in physical pixels even when an output
//! advertises scale 2. XWayland is both a Wayland client and an X server, so
//! this is the one boundary where an accidental physical/logical conversion
//! can leave the picture, the X window, and delivered button coordinates
//! describing three different rectangles. A raw x11rb client makes every
//! side observable without depending on a toolkit's own scaling policy.

use std::path::Path;
use std::time::Duration;

use chonk_testkit::{poll_until, Session, SessionOptions, WindowInfo};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateWindowAux, EventMask,
    WindowClass,
};
use x11rb::protocol::Event;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT};

const EVENT: Duration = Duration::from_secs(10);
const TITLE: &str = "xwayland-input-probe";

fn xwayland_display(session: &Session) -> u32 {
    poll_until(EVENT, "XWayland to announce its display", || {
        let log = session.log();
        let line = log.lines().find(|line| line.contains("XWayland ready"))?;
        line.split("display=").nth(1)?.trim().parse().ok()
    })
    .expect("the nested session starts XWayland")
}

fn ewmh_display(session: &Session) -> u32 {
    poll_until(EVENT, "the XWayland EWMH connection to become ready", || {
        let log = session.log();
        let line = log
            .lines()
            .find(|line| line.contains("EWMH ready on the XWayland root"))?;
        line.split("display=").nth(1)?.trim().parse().ok()
    })
    .expect("the nested session starts its XWayland EWMH connection")
}

fn intern(conn: &x11rb::rust_connection::RustConnection, name: &[u8]) -> Atom {
    conn.intern_atom(false, name).unwrap().reply().unwrap().atom
}

fn property_values(
    conn: &x11rb::rust_connection::RustConnection,
    window: u32,
    property: Atom,
    property_type: Atom,
) -> Vec<u32> {
    conn.get_property(false, window, property, property_type, 0, u32::MAX)
        .unwrap()
        .reply()
        .unwrap()
        .value32()
        .map(|values| values.collect())
        .unwrap_or_default()
}

fn resource_manager(
    conn: &x11rb::rust_connection::RustConnection,
    root: u32,
) -> Option<Vec<u8>> {
    let reply = conn
        .get_property(
            false,
            root,
            AtomEnum::RESOURCE_MANAGER,
            AtomEnum::STRING,
            0,
            u32::MAX,
        )
        .ok()?
        .reply()
        .ok()?;
    (reply.type_ == u32::from(AtomEnum::STRING) && reply.format == 8).then_some(reply.value)
}

fn assert_x11_rect(
    conn: &x11rb::rust_connection::RustConnection,
    root: u32,
    xid: u32,
    drawn: &WindowInfo,
) {
    let expected = (
        drawn.x as i16,
        drawn.y as i16,
        drawn.w as u16,
        drawn.h as u16,
    );
    let observed = poll_until(
        EVENT,
        "XWayland to apply the compositor's content rectangle",
        || {
            let geometry = conn.get_geometry(xid).ok()?.reply().ok()?;
            let translated = conn
                .translate_coordinates(xid, root, 0, 0)
                .ok()?
                .reply()
                .ok()?;
            let observed = (
                translated.dst_x,
                translated.dst_y,
                geometry.width,
                geometry.height,
            );
            (observed == expected).then_some(observed)
        },
    )
    .unwrap_or_else(|timeout| panic!("{timeout}; expected {expected:?}"));
    assert_eq!(
        observed, expected,
        "the X server and compositor must describe one content rectangle"
    );
}

fn next_button_press(conn: &x11rb::rust_connection::RustConnection) -> (i16, i16, i16, i16) {
    poll_until(EVENT, "the X client to receive a button press", || loop {
        match conn.poll_for_event() {
            Ok(Some(Event::ButtonPress(event))) => {
                return Some((event.event_x, event.event_y, event.root_x, event.root_y));
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return None,
        }
    })
    .expect("the compositor delivers the click to XWayland")
}

fn assert_clicks_match(
    session: &mut Session,
    conn: &x11rb::rust_connection::RustConnection,
    window: &WindowInfo,
) {
    let right = window.w.saturating_sub(9) as i32;
    let bottom = window.h.saturating_sub(9) as i32;
    let samples = [
        (8, 8),
        (window.w as i32 / 2, window.h as i32 / 2),
        (right, 8),
        (8, bottom),
        (right, bottom),
    ];
    for (local_x, local_y) in samples {
        let root_x = window.x + local_x;
        let root_y = window.y + local_y;
        session
            .door()
            .click(root_x as f64, root_y as f64)
            .expect("inject click");
        let delivered = next_button_press(conn);
        assert_eq!(
            delivered,
            (local_x as i16, local_y as i16, root_x as i16, root_y as i16),
            "XWayland button coordinates must match the cursor at local ({local_x}, {local_y})"
        );
    }
}

#[test]
#[ignore = "needs a Wayland session to nest inside"]
fn xwayland_geometry_and_clicks_match_at_scale_two_from_first_map() {
    let mut session = Session::boot(
        "xwayland-input-scale-two",
        SessionOptions {
            scale: Some(2.0),
            ..SessionOptions::default()
        },
    )
    .expect("nested compositor boots");
    let display = xwayland_display(&session);
    let (conn, screen_num) =
        x11rb::rust_connection::RustConnection::connect(Some(&format!(":{display}")))
            .expect("connect to nested XWayland");
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;
    let world = session.world().expect("read the compositor output");
    let root_geometry = conn.get_geometry(root).unwrap().reply().unwrap();
    assert_eq!(
        (root_geometry.width, root_geometry.height),
        (world.output_w as u16, world.output_h as u16),
        "XWayland's root must use the compositor's physical-pixel coordinate space"
    );
    let xid = conn.generate_id().unwrap();
    conn.create_window(
        COPY_DEPTH_FROM_PARENT,
        xid,
        root,
        0,
        0,
        720,
        440,
        0,
        WindowClass::INPUT_OUTPUT,
        COPY_FROM_PARENT,
        &CreateWindowAux::new()
            .background_pixel(screen.black_pixel)
            .event_mask(
                EventMask::EXPOSURE
                    | EventMask::STRUCTURE_NOTIFY
                    | EventMask::POINTER_MOTION
                    | EventMask::BUTTON_PRESS
                    | EventMask::BUTTON_RELEASE,
            ),
    )
    .unwrap();
    conn.change_property8(
        x11rb::protocol::xproto::PropMode::REPLACE,
        xid,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        TITLE.as_bytes(),
    )
    .unwrap();
    conn.change_property8(
        x11rb::protocol::xproto::PropMode::REPLACE,
        xid,
        AtomEnum::WM_CLASS,
        AtomEnum::STRING,
        b"xwayland-input-probe\0xwayland-input-probe\0",
    )
    .unwrap();
    conn.map_window(xid).unwrap();
    conn.flush().unwrap();

    // `map_window_request` creates the backend record before wm-core
    // has placed and decorated it. Waiting for any ledger entry can
    // therefore capture the pre-manage `(0, 0)` geometry and compare
    // XWayland against a stale snapshot after the real configure lands.
    // This isolated session has one application window. Its cached
    // X11 identity is not part of the test-door record, so the useful
    // observable is the record's mapped bit rather than a title match.
    let mapped = poll_until(EVENT, "the X11 window to finish mapping", || {
        session
            .world()
            .ok()?
            .windows
            .into_iter()
            .find(|window| window.mapped)
    })
    .expect("the X11 window maps");
    session.door().barrier().expect("initial configure settles");
    assert_x11_rect(&conn, root, xid, &mapped);
    while conn.poll_for_event().unwrap().is_some() {}
    assert_clicks_match(&mut session, &conn, &mapped);

    let world = session.world().expect("read the frame");
    let frame = world
        .frame_of(mapped.id)
        .expect("X11 client receives a server frame");
    let titlebar = (frame.x as f64 + frame.w as f64 / 2.0, frame.y as f64 + 8.0);
    session
        .door()
        .drag_to(titlebar, (titlebar.0 + 96.0, titlebar.1 + 64.0))
        .expect("move the X11 window");
    session
        .door()
        .button("left", false)
        .expect("release the move");
    session.door().barrier().expect("move settles");
    let moved = session
        .world()
        .expect("read the moved X11 window")
        .windows
        .into_iter()
        .next()
        .expect("it remains mapped");
    assert_x11_rect(&conn, root, xid, &moved);
    while conn.poll_for_event().unwrap().is_some() {}
    assert_clicks_match(&mut session, &conn, &moved);
}

/// XSETTINGS does not reach Java, Electron's X11 backend or Xcursor.
/// Those clients read the root resource database, so exercise the real
/// XWayland property at startup and after a live scale change. Seed a
/// user resource between the two to prove the compositor merges rather
/// than replacing the shared database.
#[test]
#[ignore = "needs a Wayland session to nest inside"]
fn xwayland_resource_manager_publishes_scale_and_preserves_user_resources() {
    let session = Session::boot(
        "xwayland-resource-manager",
        SessionOptions { scale: Some(2.0), ..SessionOptions::default() },
    )
    .expect("nested compositor boots");
    let display = xwayland_display(&session);
    let (conn, screen_num) =
        x11rb::rust_connection::RustConnection::connect(Some(&format!(":{display}")))
            .expect("connect to nested XWayland");
    let root = conn.setup().roots[screen_num].root;

    let initial = poll_until(EVENT, "the scale-2 X resource database", || {
        let bytes = resource_manager(&conn, root)?;
        let text = String::from_utf8(bytes).ok()?;
        (text.contains("Xft.dpi:\t192\n") && text.contains("Xcursor.size:\t48\n"))
            .then_some(text)
    })
    .expect("XWayland clients receive scale resources at startup");
    assert_eq!(initial.matches("Xft.dpi:").count(), 1);
    assert_eq!(initial.matches("Xcursor.size:").count(), 1);

    // Simulate `xrdb -merge`: preserve both the compositor's current
    // declarations and a user's unrelated one. The next scale change
    // must re-read this property, replace only its own keys and leave
    // the addition byte-for-byte intact.
    let mut seeded = initial.into_bytes();
    seeded.extend_from_slice(b"ChonkstepTest.keep:\tuser-value\n");
    conn.change_property8(
        x11rb::protocol::xproto::PropMode::REPLACE,
        root,
        AtomEnum::RESOURCE_MANAGER,
        AtomEnum::STRING,
        &seeded,
    )
    .unwrap();
    conn.flush().unwrap();

    session.rewrite_config("scale = 1.5\n").unwrap();
    session.request_reload().unwrap();
    let reloaded = poll_until(EVENT, "the live scale change to reach X resources", || {
        let bytes = resource_manager(&conn, root)?;
        let text = String::from_utf8(bytes).ok()?;
        (text.contains("Xft.dpi:\t144\n")
            && text.contains("Xcursor.size:\t36\n")
            && text.contains("ChonkstepTest.keep:\tuser-value\n"))
        .then_some(text)
    })
    .expect("live scale changes merge into RESOURCE_MANAGER");
    assert_eq!(reloaded.matches("Xft.dpi:").count(), 1);
    assert_eq!(reloaded.matches("Xcursor.size:").count(), 1);
    assert!(!reloaded.contains("Xft.dpi:\t192\n"));
    assert!(!reloaded.contains("Xcursor.size:\t48\n"));
}

/// A toolkit's own minimize button calls XIconifyWindow, which sends
/// `WM_CHANGE_STATE(IconicState)` to the root. Exercise that exact
/// client message against a real XWayland server, then send
/// `NormalState` back: the window must remain managed while hidden,
/// publish both EWMH and ICCCM state, and restore without its own
/// property publications feeding back into another transition.
#[test]
#[ignore = "needs a Wayland session to nest inside"]
fn xwayland_wm_change_state_minimizes_and_restores() {
    let mut session = Session::boot("xwayland-wm-change-state", SessionOptions::default())
        .expect("nested compositor boots");
    let display = ewmh_display(&session);
    let (conn, screen_num) =
        x11rb::rust_connection::RustConnection::connect(Some(&format!(":{display}")))
            .expect("connect to nested XWayland");
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;
    let xid = conn.generate_id().unwrap();
    let motif_wm_hints = intern(&conn, b"_MOTIF_WM_HINTS");
    conn.create_window(
        COPY_DEPTH_FROM_PARENT,
        xid,
        root,
        0,
        0,
        640,
        400,
        0,
        WindowClass::INPUT_OUTPUT,
        COPY_FROM_PARENT,
        &CreateWindowAux::new()
            .background_pixel(screen.black_pixel)
            .event_mask(EventMask::STRUCTURE_NOTIFY | EventMask::PROPERTY_CHANGE),
    )
    .unwrap();
    // flags = MWM_HINTS_DECORATIONS, decorations = 0: this client
    // draws its own titlebar, so its own XIconifyWindow request is the
    // only minimize button involved in the test.
    conn.change_property32(
        x11rb::protocol::xproto::PropMode::REPLACE,
        xid,
        motif_wm_hints,
        motif_wm_hints,
        &[2, 0, 0, 0, 0],
    )
    .unwrap();
    conn.change_property8(
        x11rb::protocol::xproto::PropMode::REPLACE,
        xid,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        b"xwayland-minimize-probe",
    )
    .unwrap();
    conn.map_window(xid).unwrap();
    conn.flush().unwrap();

    let managed = poll_until(EVENT, "the X11 window to finish mapping", || {
        let world = session.world().ok()?;
        let window = world.windows.iter().find(|window| window.mapped)?;
        world.frame_of(window.id).is_none().then(|| window.clone())
    })
    .expect("the client-decorated X11 window maps without a compositor frame");
    session.door().barrier().expect("initial map settles");

    let wm_change_state = intern(&conn, b"WM_CHANGE_STATE");
    let wm_state = intern(&conn, b"WM_STATE");
    let net_wm_state = intern(&conn, b"_NET_WM_STATE");
    let net_wm_state_hidden = intern(&conn, b"_NET_WM_STATE_HIDDEN");
    let send_state = |state| {
        let message = ClientMessageEvent::new(32, xid, wm_change_state, [state, 0, 0, 0, 0]);
        conn.send_event(
            false,
            root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            message,
        )
        .unwrap();
        conn.flush().unwrap();
    };

    send_state(3); // ICCCM IconicState, exactly what XIconifyWindow sends.
    poll_until(EVENT, "the X11 window to become iconic", || {
        let world = session.world().ok()?;
        let window = world.windows.iter().find(|window| window.id == managed.id)?;
        let state = property_values(&conn, xid, wm_state, wm_state);
        let net_state = property_values(&conn, xid, net_wm_state, AtomEnum::ATOM.into());
        (!window.mapped && state.first() == Some(&3) && net_state.contains(&net_wm_state_hidden))
            .then_some(())
    })
    .expect("IconicState minimizes without withdrawing the managed window");

    // Let another full dispatch pass run after both property writes.
    // Smithay deliberately does not translate its own HIDDEN update
    // into a minimize callback; if that ever changes into a loop, the
    // window will fail this stable-hidden assertion.
    session.door().barrier().expect("hidden state settles");
    let hidden = session.world().expect("read hidden state");
    assert!(
        hidden.windows.iter().any(|window| window.id == managed.id),
        "miniaturizing must not withdraw the X11 client"
    );
    assert!(hidden.windows.iter().any(|window| window.id == managed.id && !window.mapped));

    send_state(1); // ICCCM NormalState.
    poll_until(EVENT, "the X11 window to return to NormalState", || {
        let world = session.world().ok()?;
        let window = world.windows.iter().find(|window| window.id == managed.id)?;
        let state = property_values(&conn, xid, wm_state, wm_state);
        let net_state = property_values(&conn, xid, net_wm_state, AtomEnum::ATOM.into());
        (window.mapped && state.first() == Some(&1) && !net_state.contains(&net_wm_state_hidden))
            .then_some(())
    })
    .expect("NormalState restores and clears the published hidden state");
}

// ---------------------------------------------------------------------
// EWMH/ICCCM requests an X11 client sends and a scripted desktop relies
// on. Root client messages reach the compositor's own `xewmh`
// connection; `WM_HINTS` reaches it through smithay's `X11Surface`.

/// Maps a raw X11 window with a title and class, and waits for the
/// compositor to have managed it.
fn map_probe_window(
    session: &mut Session,
    conn: &x11rb::rust_connection::RustConnection,
    screen_num: usize,
    title: &str,
    hints: Option<&[u32; 9]>,
) -> (u32, u64) {
    let existing = mapped_ids(session);
    let screen = &conn.setup().roots[screen_num];
    let xid = conn.generate_id().unwrap();
    conn.create_window(
        COPY_DEPTH_FROM_PARENT,
        xid,
        screen.root,
        0,
        0,
        400,
        300,
        0,
        WindowClass::INPUT_OUTPUT,
        COPY_FROM_PARENT,
        &CreateWindowAux::new()
            .background_pixel(screen.black_pixel)
            .event_mask(EventMask::STRUCTURE_NOTIFY),
    )
    .unwrap();
    conn.change_property8(
        x11rb::protocol::xproto::PropMode::REPLACE,
        xid,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        title.as_bytes(),
    )
    .unwrap();
    // `_NET_WM_NAME` as well as `WM_NAME`: the EWMH property is what
    // smithay reads for `X11Surface::title`, and therefore what the
    // ledger matches on.
    let net_wm_name = intern(conn, b"_NET_WM_NAME");
    let utf8 = intern(conn, b"UTF8_STRING");
    conn.change_property8(
        x11rb::protocol::xproto::PropMode::REPLACE,
        xid,
        net_wm_name,
        utf8,
        title.as_bytes(),
    )
    .unwrap();
    // The ledger matches on app id or title, and an X11 window's title
    // reaches the record only through a later `TitleChanged`; the class
    // is read at map time. Naming the class after the window is what
    // makes `wait_for_window` deterministic here.
    let class = format!("{title}\0{title}\0");
    conn.change_property8(
        x11rb::protocol::xproto::PropMode::REPLACE,
        xid,
        AtomEnum::WM_CLASS,
        AtomEnum::STRING,
        class.as_bytes(),
    )
    .unwrap();
    if let Some(hints) = hints {
        // Written *before* the map, which is the ordinary case and the
        // one that produces no PropertyNotify for a change handler to
        // catch.
        conn.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            xid,
            AtomEnum::WM_HINTS,
            AtomEnum::WM_HINTS,
            hints,
        )
        .unwrap();
    }
    conn.map_window(xid).unwrap();
    conn.flush().unwrap();
    // An X11 window's identity is not part of the test-door record (see
    // the scale-two test's note), so the observable is a *new* mapped
    // entry rather than a title match.
    let id = poll_until(EVENT, "the probe window to finish mapping", || {
        session
            .world()
            .ok()?
            .windows
            .into_iter()
            .find(|window| window.mapped && !existing.contains(&window.id))
            .map(|window| window.id)
    })
    .expect("the probe window maps");
    (xid, id)
}

/// The ledger ids already mapped, so a later map can be told apart.
fn mapped_ids(session: &mut Session) -> Vec<u64> {
    session
        .world()
        .map(|world| world.windows.into_iter().filter(|w| w.mapped).map(|w| w.id).collect())
        .unwrap_or_default()
}

/// The Hyprland IPC request socket for a nested session, and one
/// request against it. Small local copies of `hyprland_ipc.rs`'s
/// helpers: this file needs exactly two calls, and the harness does not
/// export them.
fn ipc_socket_dir(session: &Session) -> std::path::PathBuf {
    let signature = poll_until(EVENT, "the hyprland ipc log line", || {
        let log = session.log();
        let at = log.find("hyprland ipc listening")?;
        let rest = &log[at..];
        let start = rest.find('"')? + 1;
        let end = rest[start..].find('"')? + start;
        Some(rest[start..end].to_string())
    })
    .expect("the compositor logs the signature it bound");
    let runtime = std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR");
    std::path::PathBuf::from(runtime).join("hypr").join(signature)
}

/// The Hyprland event socket. A bar listens to exactly this, and
/// `urgent` is the user-visible signal an attention request produces.
///
/// Held open across the whole test rather than reconnected per wait:
/// the stream carries no history, so a listener that connects after the
/// trigger has already missed it.
struct IpcEvents(std::io::BufReader<std::os::unix::net::UnixStream>);

impl IpcEvents {
    fn connect(dir: &Path) -> IpcEvents {
        let stream = std::os::unix::net::UnixStream::connect(dir.join(".socket2.sock"))
            .expect("event socket");
        stream.set_read_timeout(Some(EVENT)).expect("read timeout");
        IpcEvents(std::io::BufReader::new(stream))
    }

    fn wait_for(&mut self, name: &str) -> String {
        use std::io::BufRead as _;
        let deadline = std::time::Instant::now() + EVENT;
        let prefix = format!("{name}>>");
        let mut seen = Vec::new();
        while std::time::Instant::now() < deadline {
            let mut line = String::new();
            if self.0.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            let line = line.trim_end().to_string();
            if let Some(data) = line.strip_prefix(&prefix) {
                return data.to_string();
            }
            seen.push(line);
        }
        panic!("never saw {name}; events seen: {seen:#?}");
    }
}

fn ipc_request(dir: &Path, payload: &str) -> String {
    use std::io::{Read as _, Write as _};
    let mut stream = std::os::unix::net::UnixStream::connect(dir.join(".socket.sock"))
        .unwrap_or_else(|e| panic!("connecting to the request socket: {e}"));
    stream.set_read_timeout(Some(EVENT)).expect("read timeout");
    stream.write_all(payload.as_bytes()).expect("write the request");
    stream.flush().expect("flush");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read the response");
    response
}

/// `WM_HINTS` with only the urgency bit set. Flags is element 0; the
/// urgency bit is `1 << 8`.
const WM_HINTS_URGENT: [u32; 9] = [1 << 8, 0, 0, 0, 0, 0, 0, 0, 0];
const WM_HINTS_CALM: [u32; 9] = [0, 0, 0, 0, 0, 0, 0, 0, 0];

/// `wmctrl -c` and `xdotool windowclose` send this and nothing else.
/// Under the Wayland session it used to land on the floor: the message
/// reached the compositor's connection and `drain_inbound` dropped it,
/// so the tool exited zero having done nothing at all.
#[test]
#[ignore = "needs a live Wayland session to nest in"]
fn a_net_close_window_message_closes_an_xwayland_window() {
    let mut session =
        Session::boot("xwayland-ewmh-close", SessionOptions::default()).expect("nested compositor boots");
    let display = xwayland_display(&session);
    let _ = ewmh_display(&session);
    let (conn, screen_num) =
        x11rb::rust_connection::RustConnection::connect(Some(&format!(":{display}")))
            .expect("connect to nested XWayland");
    let root = conn.setup().roots[screen_num].root;
    let (xid, id) = map_probe_window(&mut session, &conn, screen_num, "ewmh-close-me", None);

    let close = intern(&conn, b"_NET_CLOSE_WINDOW");
    conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
        ClientMessageEvent::new(32, xid, close, [0u32, 2, 0, 0, 0]),
    )
    .unwrap();
    conn.flush().unwrap();

    // The backend's record outlives `wm-core`'s until the X11 surface
    // is destroyed, so the observable is the mapped bit — "gone from
    // the desktop", which is what the message asked for.
    poll_until(EVENT, "the window named by _NET_CLOSE_WINDOW to go", || {
        session
            .world()
            .ok()?
            .windows
            .iter()
            .all(|window| window.id != id || !window.mapped)
            .then_some(())
    })
    .expect("_NET_CLOSE_WINDOW must close the window it names");
}

/// The ICCCM half of the attention story — how IRC clients, terminal
/// bells and mail notifiers actually ask, and the half smithay's XWM
/// drops on the floor because `_NET_WM_STATE` is the only route it
/// decodes.
///
/// The urgency is raised on a window that is *not* focused, which is
/// both the realistic shape and the only one that can be observed: a
/// window that asks for attention while the user is already in it has
/// its request answered immediately, by the same dismissal this change
/// adds.
#[test]
#[ignore = "needs a live Wayland session to nest in"]
fn wm_hints_urgency_is_read_at_map_and_cleared_by_focus() {
    let mut options = SessionOptions::default();
    options.env.push(("CHONKSTEP_HYPRLAND_IPC".to_string(), "1".to_string()));
    let mut session =
        Session::boot("xwayland-ewmh-urgency", options).expect("nested compositor boots");
    let display = xwayland_display(&session);
    let _ = ewmh_display(&session);
    let (conn, screen_num) =
        x11rb::rust_connection::RustConnection::connect(Some(&format!(":{display}")))
            .expect("connect to nested XWayland");
    // A second window mapped after the noisy one, so the noisy one is
    // genuinely in the background when it asks for attention.
    let (noisy, _) = map_probe_window(&mut session, &conn, screen_num, "ewmh-noisy", Some(&WM_HINTS_CALM));
    // Mapped second, so it holds focus and the noisy one is genuinely
    // in the background when it rings.
    let _calm = map_probe_window(&mut session, &conn, screen_num, "ewmh-calm", Some(&WM_HINTS_CALM));

    // Observed on the Hyprland event socket, because that is where an
    // attention request becomes visible to a bar: the differ emits
    // `urgent>>` on the false→true edge of exactly the flag
    // `xdg_system_bell` sets for a native client.
    let dir = ipc_socket_dir(&session);
    let address_of = |dir: &Path, class: &str| -> String {
        let clients: serde_json::Value =
            serde_json::from_str(&ipc_request(dir, "j/clients")).expect("clients is JSON");
        clients
            .as_array()
            .expect("clients is an array")
            .iter()
            .find(|client| client["class"].as_str().is_some_and(|c| c.contains(class)))
            .and_then(|client| client["address"].as_str())
            .unwrap_or_else(|| panic!("no client with class {class}"))
            .to_string()
    };
    // `j/clients` prints `0x`-prefixed; the event stream does not.
    let noisy_address =
        address_of(&dir, "ewmh-noisy").trim_start_matches("0x").to_string();
    let mut events = IpcEvents::connect(&dir);

    // The bell: the client raises the ICCCM urgency bit on a window the
    // user is not looking at. Smithay parses `WM_HINTS` and reports the
    // change; before this, `property_notify` discarded it.
    conn.change_property32(
        x11rb::protocol::xproto::PropMode::REPLACE,
        noisy,
        AtomEnum::WM_HINTS,
        AtomEnum::WM_HINTS,
        &WM_HINTS_URGENT,
    )
    .unwrap();
    conn.flush().unwrap();

    assert_eq!(
        events.wait_for("urgent"),
        noisy_address,
        "a WM_HINTS urgency bit must raise the same flag xdg_system_bell does"
    );

    // And it must be dismissible. Nothing in the tree cleared this
    // before, so a window that rang once stayed urgent forever.
    let activate = intern(&conn, b"_NET_ACTIVE_WINDOW");
    let root = conn.setup().roots[screen_num].root;
    conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
        ClientMessageEvent::new(32, noisy, activate, [2u32, 0, 0, 0, 0]),
    )
    .unwrap();
    conn.flush().unwrap();

    // Wait for the focus to actually land before ringing again. The
    // differ compares successive snapshots, so the dismissal and the
    // re-ring have to fall in different passes or there is no edge
    // between them to see.
    assert_eq!(
        events.wait_for("activewindowv2"),
        noisy_address,
        "the activate must focus the window that rang"
    );

    // And the dismissal, observed the same way: the flag has to be
    // *clearable*, so ringing again after the focus must produce a
    // second `urgent` event rather than nothing. Before this change
    // nothing ever cleared it, so the second edge could never happen.
    conn.change_property32(
        x11rb::protocol::xproto::PropMode::REPLACE,
        noisy,
        AtomEnum::WM_HINTS,
        AtomEnum::WM_HINTS,
        &WM_HINTS_CALM,
    )
    .unwrap();
    conn.flush().unwrap();
    conn.change_property32(
        x11rb::protocol::xproto::PropMode::REPLACE,
        noisy,
        AtomEnum::WM_HINTS,
        AtomEnum::WM_HINTS,
        &WM_HINTS_URGENT,
    )
    .unwrap();
    conn.flush().unwrap();
    assert_eq!(
        events.wait_for("urgent"),
        noisy_address,
        "urgency must be clearable, or a window that rings once can never ring again"
    );
}
