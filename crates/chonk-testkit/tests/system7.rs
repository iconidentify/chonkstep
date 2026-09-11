//! Real Wayland and XWayland clients: pixels, input, shade and live toggling.
use std::time::Duration;
use chonk_testkit::{poll_until, FrameInfo, Screenshot, Session, SessionOptions, WindowInfo};
use wm_theme::{DecorationStyle, FontState, RasterThemeEngine};
use wm_theme_api::{DecorationRequest, Size, ThemeEngine};

const TERMINAL_SCENE: &str = r#"printf '\033[1;36mChonkStep  /  System 7\033[0m\n\nLive desktop test fixture\n\nWindow actions\n  Close  /  Zoom  /  Shade\n\nDisplay support\n  Native pixels  /  Mixed DPI\n\nConfig: decoration_style = system7\n'; exec sleep 3600"#;

fn config(scale: u32, style: &str) -> String {
    format!("scale = {scale}\ndecoration_style = \"{style}\"\ntheme = \"nextstep-classic\"\nshow_dock = false\nomarchy_shell = false\nhyprland_config = false\n")
}

fn pair(session: &mut Session, id: u64) -> (WindowInfo, FrameInfo) {
    let world = session.world().unwrap();
    (world.windows.into_iter().find(|w| w.id == id).unwrap(), world.frames.into_iter().find(|f| f.window == id).unwrap())
}

fn pixels(session: &mut Session, id: u64, scale: u32, shaded: bool, desktop: &Screenshot, name: &str) {
    pixels_on_output(session, id, scale, shaded, desktop, name, None);
}

#[allow(clippy::too_many_arguments)]
fn pixels_on_output(session: &mut Session, id: u64, scale: u32, shaded: bool, desktop: &Screenshot, name: &str,
    output: Option<(&str, i32)>) {
    session.door().motion(0.0, 0.0).unwrap();
    session.door().barrier().unwrap();
    let (window, mut frame) = pair(session, id);
    assert_eq!(frame.input_margin, 4 * scale);
    frame.x -= output.map_or(0, |(_, origin)| origin);
    let request = DecorationRequest { content_size: Size::new(window.w, if shaded { 0 } else { window.h }),
        title: window.title, focused: true, resizable: true, buttons: Vec::new() };
    let engine = RasterThemeEngine::with_fonts(wm_theme::default_theme::nextstep_classic(), FontState::new())
        .with_style(DecorationStyle::System7).unwrap();
    let mut layout = engine.layout_at(&request, scale as f32);
    if shaded { layout.frame_size.h = layout.shaded_frame_height; }
    assert_eq!((frame.w, frame.h), (layout.frame_size.w, layout.frame_size.h));
    let surface = engine.render_surface_at(&request, &layout, scale as f32);
    let capture = match output {
        Some((output, _)) => session.screenshot_output(name, output).unwrap(),
        None => session.screenshot(name).unwrap(),
    };
    // The standalone renderer is independently pinned to 84 OS-derived cases.
    // This checks all of its bytes survived real upload/composition/capture.
    for part in surface.parts {
        for y in 0..part.buffer.height {
            for x in 0..part.buffer.width {
                let at = ((y * part.buffer.width + x) * 4) as usize;
                let expected = &part.buffer.pixels[at..at + 4];
                let gx = (frame.x + part.offset.x) as u32 + x;
                let gy = (frame.y + part.offset.y) as u32 + y;
                if expected[3] == 0 {
                    assert_eq!(capture.pixel(gx, gy), desktop.pixel(gx, gy), "{name}: transparent corner ({gx},{gy})");
                } else {
                    assert_eq!(capture.pixel(gx, gy).as_slice(), expected, "{name}: chrome pixel ({gx},{gy})");
                }
            }
        }
    }
    for y in 0..frame.h {
        for x in 0..frame.w {
            if x >= 4 * scale && x < frame.w - 4 * scale && y >= 4 * scale && y < frame.h - 4 * scale { continue; }
            let gx = frame.x as u32 + x;
            let gy = frame.y as u32 + y;
            assert_eq!(capture.pixel(gx, gy), desktop.pixel(gx, gy), "{name}: input-only ring ({gx},{gy})");
        }
    }
}

fn exercise(scale: u32, x11: bool) {
    let name = format!("system7-{}-{scale}", if x11 { "xwayland" } else { "wayland" });
    let mut session = Session::boot(&name, SessionOptions {
        config_extra: config(scale, "system7").replace("omarchy_shell = false\n", ""), ..Default::default()
    }).unwrap();
    session.door().motion(0.0, 0.0).unwrap();
    let desktop = session.screenshot("desktop").unwrap();
    let app = if x11 {
        session.launch_x11_isolated("xterm", &["-class", "System7XTerm", "-title", "Terminal", "-geometry", "60x14", "-e", "sh", "-c", TERMINAL_SCENE]).unwrap();
        "System7XTerm"
    } else {
        session.launch_isolated("foot", &["--app-id", "system7-foot", "--title", "Terminal", "--window-size-pixels", "480x260", "sh", "-c", TERMINAL_SCENE]).unwrap();
        "system7-foot"
    };
    let window = session.wait_for_window(app).unwrap();
    let id = window.id;
    let (_, frame) = pair(&mut session, id);
    let title = (f64::from(frame.x) + f64::from(frame.w / 2), f64::from(frame.y) + f64::from(14 * scale));
    session.door().drag_to(title, (title.0 + 80.0 - f64::from(frame.x), title.1 + 80.0 - f64::from(frame.y))).unwrap();
    session.door().button("left", false).unwrap();
    pixels(&mut session, id, scale, false, &desktop, "initial");

    // Repeat both directions without restarting the compositor or the client.
    let initial = pair(&mut session, id).0;
    for cycle in 0..3 {
        for style in ["windowmaker", "system7"] {
            let configures = session.world().unwrap().spatial.configures;
            session.rewrite_config(&config(scale, style)).unwrap();
            session.request_reload().unwrap();
            poll_until(Duration::from_secs(10), "live style metrics", || {
                let (w, f) = pair(&mut session, id);
                let expected = if style == "system7" { 23 * scale as i32 } else {
                    let theme = wm_theme::default_theme::nextstep_classic();
                    (u32::from(theme.titlebar.height) + u32::from(theme.border.width)) as i32 * scale as i32
                };
                (w.y - f.y == expected).then_some(())
            }).unwrap();
            let current = pair(&mut session, id).0;
            assert_eq!((current.x, current.y, current.w, current.h), (initial.x, initial.y, initial.w, initial.h));
            assert_eq!((current.workspace, current.stack_index, current.mapped),
                (initial.workspace, initial.stack_index, initial.mapped));
            let world = session.world().unwrap();
            assert_eq!(world.theme.decoration_style, style);
            // Core issues one configure request per frame. Smithay can omit
            // an identical size on the wire; neither case permits a storm.
            assert!(world.spatial.configures - configures <= 1);
        }
        pixels(&mut session, id, scale, false, &desktop, &format!("reload-{cycle}"));
    }

    // Grow by dragging the black right shadow, then by its transparent ring.
    for ring in [false, true] {
        let (window, frame) = pair(&mut session, id);
        let from = (f64::from(frame.x) + f64::from(frame.w - if ring { 2 * scale } else { 5 * scale }),
            f64::from(window.y) + f64::from(window.h / 2));
        session.door().drag_to(from, (from.0 + f64::from(48 * scale), from.1)).unwrap();
        session.door().button("left", false).unwrap();
        poll_until(Duration::from_secs(10), "shadow/ring resize answered", || {
            let current = pair(&mut session, id).0;
            (current.w > window.w && current.w == current.presented_w).then_some(())
        }).unwrap_or_else(|error| panic!("{name} ring={ring}: {error}; before={window:?}; after={:?}", pair(&mut session, id)));
    }
    pixels(&mut session, id, scale, false, &desktop, "resized");
    let (_, frame) = pair(&mut session, id);
    let title = (f64::from(frame.x) + f64::from(70 * scale), f64::from(frame.y) + f64::from(14 * scale));
    session.door().click(title.0, title.1).unwrap();
    session.door().click(title.0, title.1).unwrap();
    assert!(!pair(&mut session, id).0.mapped);
    pixels(&mut session, id, scale, true, &desktop, "shaded");
    session.door().click(title.0, title.1).unwrap();
    session.door().click(title.0, title.1).unwrap();
    assert!(pair(&mut session, id).0.mapped);
    pixels(&mut session, id, scale, false, &desktop, "unshaded");

    let (before, frame) = pair(&mut session, id);
    let zoom = (f64::from(frame.x) + f64::from(frame.w - 22 * scale), f64::from(frame.y) + f64::from(13 * scale));
    session.door().click(zoom.0, zoom.1).unwrap();
    poll_until(Duration::from_secs(10), "zoom maximizes", || (pair(&mut session, id).0.w > before.w).then_some(())).unwrap();
    let (_, frame) = pair(&mut session, id);
    let zoom = (f64::from(frame.x) + f64::from(frame.w - 22 * scale), f64::from(frame.y) + f64::from(13 * scale));
    session.door().click(zoom.0, zoom.1).unwrap();
    poll_until(Duration::from_secs(10), "zoom restores", || (pair(&mut session, id).0.w == before.w).then_some(())).unwrap();
    let (_, frame) = pair(&mut session, id);
    session.door().click(f64::from(frame.x) + f64::from(18 * scale), f64::from(frame.y) + f64::from(13 * scale)).unwrap();
    poll_until(Duration::from_secs(10), "close destroys client and frame", || {
        let world = session.world().ok()?;
        (!world.windows.iter().any(|w| w.id == id) && !world.frames.iter().any(|f| f.window == id)).then_some(())
    }).unwrap();
}

#[test]
#[ignore = "real nested clients: scripts/e2e.sh --headless --test system7"]
fn system7_pixels_buttons_resize_and_repeated_live_toggle_on_wayland_and_xwayland() {
    assert!(chonk_testkit::require_client("foot"));
    assert!(chonk_testkit::require_client("xterm"));
    for scale in [1, 2] { for x11 in [false, true] { exercise(scale, x11); } }
}

#[test]
#[ignore = "real mixed-DPI outputs: scripts/e2e.sh --headless --test system7"]
fn system7_crosses_mixed_dpi_outputs_without_resampling_its_chrome() {
    let mut session = Session::boot("system7-mixed-dpi", SessionOptions {
        config_extra: config(1, "system7").replace("omarchy_shell = false\n", ""), ..Default::default()
    }).unwrap();
    session.door().virtual_outputs(true).unwrap();
    session.launch("wlr-randr", &["--output", "chonkstep-right", "--scale", "2"]).unwrap();
    let status = poll_until(Duration::from_secs(10), "set second output scale", || session.client_status("wlr-randr").ok().flatten()).unwrap();
    assert!(status.success(), "wlr-randr failed: {status}");
    session.door().motion(150.0, 200.0).unwrap();
    let desktop_left = session.screenshot_output("desktop-left", "chonkstep").unwrap();
    let desktop_right = session.screenshot_output("desktop-right", "chonkstep-right").unwrap();
    session.launch_isolated("foot", &["--app-id", "mixed-system7", "--title", "Terminal", "--window-size-pixels", "240x120", "sleep", "3600"]).unwrap();
    let id = session.wait_for_window("mixed-system7").unwrap().id;
    for (stage, scale, x) in [("left", 1, 70), ("right", 2, 720), ("left-again", 1, 70), ("right-again", 2, 720)] {
        let (window, frame) = pair(&mut session, id);
        let from = (f64::from(frame.x) + f64::from(frame.w / 2), f64::from(frame.y + (window.y - frame.y) / 2));
        session.door().drag_to(from, (from.0 + f64::from(x - frame.x), from.1 + f64::from(100 - frame.y))).unwrap();
        session.door().button("left", false).unwrap();
        poll_until(Duration::from_secs(10), "destination decoration scale and presented content", || {
            let (window, frame) = pair(&mut session, id);
            (window.y - frame.y == 23 * scale as i32 && window.w == window.presented_w).then_some(())
        }).unwrap();
        let (output, origin, desktop) = if scale == 1 { ("chonkstep", 0, &desktop_left) }
            else { ("chonkstep-right", 640, &desktop_right) };
        pixels_on_output(&mut session, id, scale, false, desktop, stage, Some((output, origin)));
    }
}
