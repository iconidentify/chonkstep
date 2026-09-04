//! XWayland geometry and pointer coordinates against a real nested session.
//!
//! The compositor stores and renders in physical pixels even when an output
//! advertises scale 2. XWayland is both a Wayland client and an X server, so
//! this is the one boundary where an accidental physical/logical conversion
//! can leave the picture, the X window, and delivered button coordinates
//! describing three different rectangles. A raw x11rb client makes every
//! side observable without depending on a toolkit's own scaling policy.

use std::time::Duration;

use chonk_testkit::{poll_until, Session, SessionOptions, WindowInfo};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, WindowClass,
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

fn assert_x11_rect(
    conn: &x11rb::rust_connection::RustConnection,
    root: u32,
    xid: u32,
    drawn: &WindowInfo,
) {
    let geometry = conn.get_geometry(xid).unwrap().reply().unwrap();
    let translated = conn
        .translate_coordinates(xid, root, 0, 0)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(
        (
            translated.dst_x,
            translated.dst_y,
            geometry.width,
            geometry.height
        ),
        (
            drawn.x as i16,
            drawn.y as i16,
            drawn.w as u16,
            drawn.h as u16
        ),
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

    let mapped = poll_until(
        EVENT,
        "the X11 window to enter the compositor ledger",
        || session.world().ok()?.windows.into_iter().next(),
    )
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
