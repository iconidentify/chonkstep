//! Named cursor shapes (`wp_cursor_shape_v1`) drawn from the Xcursor theme.
//!
//! GTK 4, Qt 6, Chromium, SDL 3 and the terminals name a shape instead of
//! committing a cursor surface, and expect the compositor to draw it. The
//! probe names one the same way, and the pixels come back through screencopy
//! with the cursor overlay. The fixture theme (`tests/fixtures/xcursor`)
//! draws each shape as an opaque square in its own colour, 24, 36 and 48 px
//! on a side, with one white pixel at the hotspot `(side / 4, side / 2)`. One
//! screenshot therefore says which shape was drawn, at which size, and where
//! its hotspot landed.
//!
//! "Today's arrow" is never described here; it is captured. A probe that
//! never names a shape is hovered first, over the same uniform content every
//! later probe draws, and later screenshots are compared with that crop.

use std::path::PathBuf;
use std::time::Duration;

use chonk_testkit::{poll_until, profile_binary, Screenshot, Session, SessionOptions};

const PROBE: &str = "chonk-input-probe";
const THEME: &str = "chonkstep-fixture";
const TEXT: [u8; 3] = [0xF0, 0xE0, 0x20];
const POINTER: [u8; 3] = [0x20, 0x50, 0xE0];
const HOTSPOT: [u8; 3] = [0xFF, 0xFF, 0xFF];
const WAIT: Duration = Duration::from_secs(10);
/// Where every hover lands inside a probe's content, in its logical units.
const HOVER: (f64, f64) = (80.0, 70.0);
/// The side of the square compared around the pointer: room for the arrow
/// and the resize double-arrow at scale 2, and for the 48 px fixture image.
const CROP: i32 = 64;
/// The developer's own session sets these, and they would otherwise pick
/// the theme and the size under test by being inherited.
const CURSOR_ENV: [&str; 4] = ["XCURSOR_THEME", "XCURSOR_PATH", "XCURSOR_SIZE", "CHONKSTEP_OWNS_XCURSOR_SIZE"];

fn fixture_path() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/xcursor").display().to_string()
}

fn fixture_env() -> Vec<(&'static str, String)> {
    vec![("XCURSOR_THEME", THEME.to_string()), ("XCURSOR_PATH", fixture_path())]
}

fn boot(name: &str, scale: f32, env: &[(&'static str, String)]) -> Session {
    let options = SessionOptions {
        scale: Some(scale),
        config_extra: "show_dock = false\n".into(),
        env_remove: CURSOR_ENV.iter().map(|name| name.to_string()).collect(),
        env: env.iter().map(|(name, value)| (name.to_string(), value.clone())).collect(),
        ..SessionOptions::default()
    };
    Session::boot(name, options).expect("nested compositor")
}

/// A mapped probe: its app id, window id, and the physical point at
/// [`HOVER`] inside its content.
struct Probe {
    app: String,
    window: u64,
    at: (i32, i32),
}

fn launch(session: &mut Session, scale: f32, app: &str, extra: &[&str]) -> Probe {
    let binary = profile_binary(PROBE).expect("input probe built");
    let scale_arg = scale.to_string();
    let app_arg = format!("--app-id={app}");
    let mut args = vec![scale_arg.as_str(), app_arg.as_str()];
    args.extend_from_slice(extra);
    session.launch(binary.to_str().unwrap(), &args).expect("probe launches");
    let window = session.wait_for_window(app).expect("probe maps");
    session.door().barrier().unwrap();
    let scale = f64::from(scale);
    let at = (
        window.x - window.offset_x + (HOVER.0 * scale).round() as i32,
        window.y - window.offset_y + (HOVER.1 * scale).round() as i32,
    );
    Probe { app: app.to_string(), window: window.id, at }
}

fn close(session: &mut Session, probe: &Probe) {
    session.kill_client(PROBE);
    session.wait_for_window_gone(&probe.app).expect("probe unmaps");
    session.door().barrier().unwrap();
}

fn hover(session: &mut Session, at: (i32, i32)) {
    session.door().motion(f64::from(at.0), f64::from(at.1)).unwrap();
    session.door().barrier().unwrap();
}

/// Waits until the compositor has processed the newest probe's `set_shape`.
fn wait_shape_applied(session: &Session) {
    poll_until(WAIT, "the probe's set_shape to be processed", || {
        session.client_log(PROBE).contains("cursor-shape applied").then_some(())
    })
    .unwrap_or_else(|error| panic!("{error}\n{}", session.client_log(PROBE)));
}

fn crop(shot: &Screenshot, at: (i32, i32)) -> Vec<[u8; 4]> {
    let (left, top) = (at.0 - CROP / 2, at.1 - CROP / 2);
    (top..top + CROP).flat_map(|y| (left..left + CROP).map(move |x| shot.pixel(x as u32, y as u32))).collect()
}

fn capture(session: &mut Session, label: &str, at: (i32, i32)) -> (Vec<[u8; 4]>, PathBuf) {
    session.door().barrier().unwrap();
    let shot = session.screenshot_with_cursor(label).unwrap();
    (crop(&shot, at), shot.path)
}

/// Per-channel slack when matching a fixture colour. grim reads a
/// fractional-scale output through its logical size, which blends a few
/// percent of each neighbour into a pixel: a white hotspot beside a red
/// body came back as (254, 248, 248) at 1.5x. The fixture's colours are
/// far further apart from each other, from the probe's red content and
/// from the wallpaper than this, and none has equal red and blue, so a
/// swapped byte order would not pass either.
const TOLERANCE: u8 = 16;

fn close_to(pixel: [u8; 4], rgb: [u8; 3]) -> bool {
    pixel.iter().zip(rgb).all(|(channel, wanted)| channel.abs_diff(wanted) <= TOLERANCE)
}

/// Whether the fixture's `side` px image in `body` is drawn with its hotspot
/// on `at`: the white hotspot pixel under the pointer, the square's four
/// corners where that hotspot puts them, and nothing of it one pixel beyond.
fn fixture_drawn(shot: &Screenshot, at: (i32, i32), side: i32, body: [u8; 3]) -> Result<(), String> {
    let origin = (at.0 - side / 4, at.1 - side / 2);
    let pixel = |(x, y): (i32, i32)| shot.pixel(x as u32, y as u32);
    let mut problems = Vec::new();
    if !close_to(pixel(at), HOTSPOT) {
        problems.push(format!("the pixel under the pointer {at:?} is {:?}, not the hotspot", pixel(at)));
    }
    let last = side - 1;
    let inside = [
        origin,
        (origin.0 + last, origin.1),
        (origin.0, origin.1 + last),
        (origin.0 + last, origin.1 + last),
        (at.0 + 1, at.1),
        (at.0, at.1 + 1),
    ];
    for point in inside {
        if !close_to(pixel(point), body) {
            problems.push(format!("{point:?} is {:?}, not the {side} px image's {body:?}", pixel(point)));
        }
    }
    let outside = [(origin.0 - 1, origin.1), (origin.0, origin.1 - 1), (origin.0 + side, origin.1 + last), (origin.0 + last, origin.1 + side)];
    for point in outside {
        if close_to(pixel(point), body) {
            problems.push(format!("{point:?}, outside a {side} px image, is still {body:?}"));
        }
    }
    if problems.is_empty() { Ok(()) } else { Err(problems.join("; ")) }
}

/// Polls screenshots until the fixture image shows: the worker delivers the
/// theme asynchronously, and the arrow is correct until it does.
fn wait_fixture(session: &mut Session, label: &str, at: (i32, i32), side: i32, body: [u8; 3]) {
    let mut last = String::new();
    let found = poll_until(WAIT, &format!("the fixture's {side} px {label} cursor"), || {
        session.door().barrier().ok()?;
        let shot = session.screenshot_with_cursor(label).ok()?;
        match fixture_drawn(&shot, at, side, body) {
            Ok(()) => Some(()),
            Err(problems) => {
                last = format!("{problems} ({})", shot.path.display());
                None
            }
        }
    });
    if let Err(error) = found {
        panic!("{error}: {last}");
    }
}

/// The arrow a client that never named a shape gets, captured over probe
/// content at `scale`. Leaves no probe behind.
fn arrow_reference(session: &mut Session, scale: f32) -> Vec<[u8; 4]> {
    let plain = launch(session, scale, "plain-probe", &[]);
    hover(session, plain.at);
    let (arrow, _) = capture(session, "arrow-reference", plain.at);
    close(session, &plain);
    arrow
}

fn named_shapes_follow_the_theme_at(scale: f32, label: &str) {
    let mut session = boot(&format!("cursor-shape-{label}"), scale, &fixture_env());
    let side = (24.0 * scale).round() as i32;
    let arrow = arrow_reference(&mut session, scale);

    for (shape, body) in [("text", TEXT), ("pointer", POINTER)] {
        let probe = launch(&mut session, scale, &format!("{shape}-probe"), &["cursor-shape", shape]);
        hover(&mut session, probe.at);
        wait_shape_applied(&session);
        wait_fixture(&mut session, shape, probe.at, side, body);
        close(&mut session, &probe);
    }

    // After `pointer` above, so the theme is known to be loaded: `default`
    // is ChonkStep's own arrow, not the theme's green square.
    let probe = launch(&mut session, scale, "default-probe", &["cursor-shape", "default"]);
    hover(&mut session, probe.at);
    wait_shape_applied(&session);
    let (drawn, path) = capture(&mut session, "default", probe.at);
    assert!(drawn == arrow, "`default` must draw today's arrow at scale {scale} ({})", path.display());
}

#[test]
#[ignore = "needs a Wayland session to nest inside"]
fn named_shapes_follow_the_theme_unscaled() {
    named_shapes_follow_the_theme_at(1.0, "1x");
}

#[test]
#[ignore = "needs a Wayland session to nest inside"]
fn named_shapes_follow_the_theme_at_a_fractional_scale() {
    named_shapes_follow_the_theme_at(1.5, "1.5x");
}

#[test]
#[ignore = "needs a Wayland session to nest inside"]
fn named_shapes_follow_the_theme_at_scale_two() {
    named_shapes_follow_the_theme_at(2.0, "2x");
}

/// A named status outlives the pointer's visit. Over a frame's resize edge
/// and over the bare desktop the compositor's own sprites must still be drawn,
/// exactly as they were before the client named anything.
#[test]
#[ignore = "needs a Wayland session to nest inside"]
fn frames_and_the_desktop_keep_the_compositor_sprites_while_a_client_names_a_shape() {
    let mut session = boot("cursor-shape-frames", 1.0, &fixture_env());
    let world = session.world().unwrap();
    let desktop = (world.output_w as i32 - 80, world.output_h as i32 - 80);
    hover(&mut session, desktop);
    let (desktop_arrow, _) = capture(&mut session, "desktop-reference", desktop);

    let probe = launch(&mut session, 1.0, "frame-probe", &["cursor-shape", "text", "resizable"]);
    assert_eq!(session.door().hit(desktop.0, desktop.1).unwrap(), "root", "the desktop point stays bare");
    let world = session.world().unwrap();
    let frame = world.frame_of(probe.window).expect("the probe is framed");
    let corner = (frame.x + frame.w as i32 - 1, frame.y + frame.h as i32 - 1);
    let mut edge = None;
    for inset in 0..24 {
        let point = (corner.0 - inset, corner.1 - inset);
        if session.door().hit(point.0, point.1).unwrap() == "frame" {
            edge = Some(point);
            break;
        }
    }
    let edge = edge.expect("a frame edge near the frame's bottom-right corner");
    hover(&mut session, edge);
    assert!(
        !session.client_log(PROBE).contains("cursor-shape applied"),
        "the references are taken before the client names a shape"
    );
    let (edge_sprite, _) = capture(&mut session, "edge-reference", edge);

    hover(&mut session, probe.at);
    wait_shape_applied(&session);
    wait_fixture(&mut session, "text", probe.at, 24, TEXT);

    hover(&mut session, edge);
    let (drawn, path) = capture(&mut session, "edge-after-named", edge);
    assert!(drawn == edge_sprite, "the frame edge keeps the compositor's sprite ({})", path.display());
    hover(&mut session, desktop);
    let (drawn, path) = capture(&mut session, "desktop-after-named", desktop);
    assert!(drawn == desktop_arrow, "the desktop keeps the compositor's arrow ({})", path.display());
}

/// Any shape, when the theme cannot be found, is today's arrow. The log
/// line is the proof the theme was looked for and came up empty, rather
/// than never consulted.
fn an_unavailable_theme_draws_the_arrow(name: &str, env: Vec<(&'static str, String)>) {
    let mut session = boot(name, 1.0, &env);
    let arrow = arrow_reference(&mut session, 1.0);
    let probe = launch(&mut session, 1.0, "text-probe", &["cursor-shape", "text"]);
    hover(&mut session, probe.at);
    wait_shape_applied(&session);
    poll_until(WAIT, "the theme worker to report an empty theme", || {
        session.log().contains("the cursor theme provides no named cursor shapes").then_some(())
    })
    .unwrap_or_else(|error| panic!("{error}\n{}", session.log()));
    let (drawn, path) = capture(&mut session, "text-without-theme", probe.at);
    assert!(drawn == arrow, "a shape the theme cannot supply draws today's arrow ({})", path.display());
}

#[test]
#[ignore = "needs a Wayland session to nest inside"]
fn a_missing_theme_path_draws_the_arrow() {
    let missing = format!("{}/does-not-exist", fixture_path());
    an_unavailable_theme_draws_the_arrow(
        "cursor-shape-missing-path",
        vec![("XCURSOR_THEME", THEME.to_string()), ("XCURSOR_PATH", missing)],
    );
}

/// With no `XCURSOR_PATH` the theme is searched for in the standard icon
/// directories, where the fixture theme is not installed.
#[test]
#[ignore = "needs a Wayland session to nest inside"]
fn an_unset_theme_path_draws_the_arrow() {
    an_unavailable_theme_draws_the_arrow("cursor-shape-unset-path", vec![("XCURSOR_THEME", THEME.to_string())]);
}

/// A user who pinned `XCURSOR_SIZE=48` gets the theme's 48 px image at
/// scale 1, not the 24 px one the default size would pick.
#[test]
#[ignore = "needs a Wayland session to nest inside"]
fn a_pinned_cursor_size_draws_the_larger_image() {
    let mut env = fixture_env();
    env.push(("XCURSOR_SIZE", "48".to_string()));
    let mut session = boot("cursor-shape-pinned-48", 1.0, &env);
    let probe = launch(&mut session, 1.0, "text-probe", &["cursor-shape", "text"]);
    hover(&mut session, probe.at);
    wait_shape_applied(&session);
    wait_fixture(&mut session, "text-48", probe.at, 48, TEXT);
}
