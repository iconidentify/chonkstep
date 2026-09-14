//! A client that draws its own titlebar wears edge chrome: this desktop's
//! borders and resize handles around it, and no titlebar of ours. The probe
//! declares itself the way a GTK4 header-bar window does, binding KDE's
//! decoration manager and creating no decoration object, and draws a
//! GTK-style shadow buffer with an input region, so the edge frame meets a
//! real surface tree at integer and fractional scales.

use std::time::Duration;

use chonk_testkit::{poll_until, profile_binary, FrameInfo, Session, SessionOptions, WindowInfo};

const EVENT: Duration = Duration::from_secs(10);
const PROBE: &str = "chonk-input-probe";
const APP: &str = "edge-frame-probe";
/// What every config in this file carries beside its `[decorations]`.
const BASE: &str = "show_dock = false\nomarchy_menu = false\nhyprland_config = false\n";

fn window(session: &mut Session) -> WindowInfo {
    session.world().unwrap().window_matching(APP).unwrap().clone()
}

fn frame(session: &mut Session, window: &WindowInfo) -> Option<FrameInfo> {
    session.world().unwrap().frame_of(window.id).cloned()
}

/// The visible chrome on each side of `window`, input margin excluded:
/// (left, top, right, bottom), in the ledger's physical pixels.
fn chrome(frame: &FrameInfo, window: &WindowInfo) -> (i32, i32, i32, i32) {
    let margin = frame.input_margin as i32;
    let (x, y) = (frame.x + margin, frame.y + margin);
    let (right, bottom) = (frame.x + frame.w as i32 - margin, frame.y + frame.h as i32 - margin);
    (
        window.x - x,
        window.y - y,
        right - (window.x + window.w as i32),
        bottom - (window.y + window.h as i32),
    )
}

fn boot(scale: f32) -> (Session, WindowInfo) {
    let mut session = Session::boot(
        &format!("client-edge-frames-{scale}"),
        SessionOptions {
            scale: Some(scale),
            config_extra: BASE.to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    let binary = profile_binary(PROBE).unwrap();
    session
        .launch_isolated(
            binary.to_str().unwrap(),
            &[
                &scale.to_string(),
                "--csd-input-region",
                "--kde-bind-only",
                "resizable",
                &format!("--app-id={APP}"),
            ],
        )
        .unwrap();
    session.wait_for_window(APP).unwrap();
    let settled = poll_until(EVENT, "committed window geometry inside its edge frame", || {
        let w = window(&mut session);
        let framed = frame(&mut session, &w).is_some_and(|frame| frame.mapped);
        (w.w == (340.0 * scale) as u32 && framed).then_some(w)
    })
    .unwrap_or_else(|error| panic!("scale={scale}: {error}\n{}", session.log()));
    (session, settled)
}

/// Rewrites the `[decorations]` table and waits for the session to reload
/// it, which re-decides the chrome of every window already open.
fn reload_decorations(session: &mut Session, scale: f32, decorations: &str) {
    let reloads = session.log().matches("reload requested").count();
    session
        .rewrite_config(&format!("scale = {scale}\n{BASE}[decorations]\n{decorations}"))
        .unwrap();
    session.request_reload().unwrap();
    poll_until(EVENT, "decoration reload", || {
        (session.log().matches("reload requested").count() > reloads).then_some(())
    })
    .unwrap();
    session.door().barrier().unwrap();
}

fn exercise_edge_frames(scale: f32) {
    let (mut session, w) = boot(scale);

    // Borders on every side, and a top edge no taller than the sides: the
    // client's header bar is the only titlebar.
    let edges = frame(&mut session, &w).unwrap();
    let (left, top, right, bottom) = chrome(&edges, &w);
    assert!(
        left > 0 && right > 0 && bottom > 0,
        "scale={scale}: borders on every side, got {:?}",
        (left, top, right, bottom)
    );
    assert_eq!(top, left, "scale={scale}: the top is a border like the sides, not a titlebar");
    assert_eq!(right, left, "scale={scale}: the sides match");

    // The edge frame's own east band resizes the window. GTK has no
    // resize band of its own once it is tiled, so this is the only one.
    let grip = (
        f64::from(edges.x + edges.w as i32 - 2),
        f64::from(w.y + w.h as i32 / 2),
    );
    assert_eq!(
        session.door().hit(grip.0 as i32, grip.1 as i32).unwrap(),
        "frame",
        "scale={scale}: the band outside the east border is ours"
    );
    session.door().drag_to(grip, (grip.0 + 60.0, grip.1)).unwrap();
    let resized = poll_until(EVENT, "resize from the edge frame", || {
        let now = window(&mut session);
        (now.w > w.w).then_some(now)
    })
    .unwrap_or_else(|error| panic!("scale={scale}: {error}\n{}", session.log()));
    session.door().button("left", false).unwrap();
    session.door().barrier().unwrap();
    assert_eq!(
        (resized.w - w.w, resized.h),
        (60, w.h),
        "scale={scale}: an east drag grows only the width, by exactly the drag"
    );
    assert_eq!((resized.x, resized.y), (w.x, w.y), "scale={scale}: the west edge stays put");

    // frame_client_drawn: the same silent client wears our titlebar as well.
    reload_decorations(&mut session, scale, "frame_client_drawn = true\n");
    let (titled, titled_window) = poll_until(EVENT, "a titlebar from frame_client_drawn", || {
        let now = window(&mut session);
        let framed = frame(&mut session, &now)?;
        let (side, top, _, _) = chrome(&framed, &now);
        (top > side + (10.0 * scale) as i32).then_some((framed, now))
    })
    .unwrap_or_else(|error| panic!("scale={scale}: {error}\n{}", session.log()));
    assert!(titled.mapped, "scale={scale}");
    let (_, full_top, _, full_bottom) = chrome(&titled, &titled_window);
    assert!(
        full_top + full_bottom > top + bottom,
        "scale={scale}: the full frame is taller than the edges it replaced"
    );

    // client_side outranks frame_client_drawn and every client signal:
    // no chrome at all, and the window stays mapped without it.
    reload_decorations(&mut session, scale, &format!("client_side = [\"{APP}\"]\nframe_client_drawn = true\n"));
    poll_until(EVENT, "client_side to take the frame away", || {
        let now = window(&mut session);
        frame(&mut session, &now).is_none().then_some(())
    })
    .unwrap_or_else(|error| panic!("scale={scale}: {error}\n{}", session.log()));
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test client_edge_frames"]
fn a_header_bar_client_wears_resizable_edges_at_1x() {
    exercise_edge_frames(1.0);
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test client_edge_frames"]
fn a_header_bar_client_wears_resizable_edges_at_fractional_scale() {
    exercise_edge_frames(1.5);
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test client_edge_frames"]
fn a_header_bar_client_wears_resizable_edges_at_2x() {
    exercise_edge_frames(2.0);
}

/// The real thing, where it is installed: Nautilus's header bar is the only
/// titlebar, inside borders that are ours.
#[test]
#[ignore = "scripts/e2e.sh --headless --test client_edge_frames"]
fn nautilus_wears_edges_around_its_header_bar() {
    if !chonk_testkit::require_client("nautilus") {
        return;
    }
    let mut session = Session::boot(
        "client-edge-frames-nautilus",
        SessionOptions { config_extra: BASE.to_string(), ..Default::default() },
    )
    .unwrap();
    let folder = session.dir.join("edge-frames");
    std::fs::create_dir(&folder).unwrap();
    session
        .launch_isolated(
            "env",
            &[
                "GSETTINGS_BACKEND=memory",
                "XDG_SESSION_TYPE=wayland",
                "GTK_A11Y=none",
                "nautilus",
                "--new-window",
                folder.to_str().unwrap(),
            ],
        )
        .unwrap();
    session.wait_for_window("edge-frames").unwrap();
    let (edges, window) = poll_until(EVENT, "Nautilus's edge frame", || {
        let world = session.world().ok()?;
        let window = world.window_matching("edge-frames")?.clone();
        let frame = world.frame_of(window.id)?.clone();
        frame.mapped.then_some((frame, window))
    })
    .unwrap_or_else(|error| panic!("{error}\n{}", session.log()));
    let (left, top, right, bottom) = chrome(&edges, &window);
    assert!(left > 0 && right > 0 && bottom > 0, "borders on every side, got {:?}", (left, top, right, bottom));
    assert_eq!(top, left, "Nautilus's header bar is the only titlebar");
}
