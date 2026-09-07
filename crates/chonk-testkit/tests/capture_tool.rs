//! Real keyboard/pointer entry, GPU pixels, clipboard and ffmpeg lifecycle.
//! Isolated directories and Wayland sockets; never touches the user's clipboard.
#![allow(clippy::disallowed_methods)] // Test process, not the compositor loop.
use chonk_testkit::{poll_until, profile_binary, session_dir, Screenshot, Session, SessionOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

fn shortcut(session: &mut Session, number: u32) {
    session.door().key(125, true).unwrap();
    session.door().key(29, true).unwrap();
    session.door().key(42, true).unwrap();
    session.door().tap_key(number + 1).unwrap();
    session.door().key(42, false).unwrap();
    session.door().key(29, false).unwrap();
    session.door().key(125, false).unwrap();
    session.door().barrier().unwrap();
}

fn files(directory: &Path, extension: &str) -> Vec<PathBuf> {
    let mut paths: Vec<_> = std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            !p.file_name().unwrap().to_string_lossy().starts_with('.')
                && p.extension().is_some_and(|e| e == extension)
        })
        .collect();
    paths.sort();
    paths
}

fn saved(directory: &Path, count: usize, extension: &str) -> PathBuf {
    poll_until(Duration::from_secs(30), "capture saved", || {
        let found = files(directory, extension);
        (found.len() == count).then(|| found.last().unwrap().clone())
    })
    .unwrap()
}

fn boot(name: &str, scale: f32) -> Session {
    let root = session_dir(name);
    let dir = root.join("exports");
    let fixture_bin = root.join("config/chonkstep/fixture-bin");
    let mut search_path = vec![fixture_bin.clone()];
    search_path.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    // The compositor inherits these wrappers before its worker can start.
    // Never launch real review apps against the developer's desktop in E2E.
    // Record argument boundaries and whether publication preceded the launch.
    let wrapper = "#!/bin/sh\npublished=missing\nif [ \"$#\" -eq 1 ] && [ -s \"$1\" ]; then published=published; fi\nprintf '%s\\0' \"${0##*/}\" \"$#\" \"$@\" \"$published\" >> \"$CHONK_TEST_CAPTURE_REVIEW_LOG\"\n";
    let session = Session::boot(
        name,
        SessionOptions {
            scale: Some(scale),
            config_extra: "desktop = \"omarchy\"\nomarchy_bar = false\nshow_dock = false\n".into(),
            config_root_files: vec![(
                "hypr/hyprland.conf".into(),
                "bind = SUPER, F12, workspace, 1\nbind = , PRINT, exec, omarchy-capture-screenshot\n".into(),
            )],
            config_files: ["xdg-open", "omacut"]
                .into_iter()
                .map(|program| (format!("fixture-bin/{program}"), wrapper.into()))
                .chain(std::iter::once(("fixture-bin/notify-send".into(), "#!/bin/sh\nexit 0\n".into())))
                .collect(),
            env: vec![
                (
                    "PATH".into(),
                    std::env::join_paths(search_path).unwrap().into_string().unwrap(),
                ),
                ("CHONKSTEP_HYPRLAND_IPC".into(), "1".into()),
                (
                    "CHONK_TEST_CAPTURE_REVIEW_LOG".into(),
                    root.join("capture-review.log").display().to_string(),
                ),
                ("OMARCHY_SCREENSHOT_DIR".into(), dir.display().to_string()),
                ("OMARCHY_SCREENRECORD_DIR".into(), dir.display().to_string()),
            ],
            ..SessionOptions::default()
        },
    )
    .unwrap();
    // Session::boot recreates the session directory, so chmod must happen
    // after boot and before sending the first capture shortcut.
    for program in ["xdg-open", "omacut", "notify-send"] {
        std::fs::set_permissions(
            fixture_bin.join(program),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
    session
}

fn reviews_opened(session: &Session, expected: &[(&str, &Path)]) {
    let mut bytes = Vec::new();
    for (program, path) in expected {
        for field in [
            program.as_bytes(),
            b"1",
            path.as_os_str().as_bytes(),
            b"published",
        ] {
            bytes.extend_from_slice(field);
            bytes.push(0);
        }
    }
    poll_until(
        Duration::from_secs(10),
        "published capture opens in its review app",
        || {
            (std::fs::read(session.dir.join("capture-review.log"))
                .ok()?
                .as_slice()
                == bytes)
                .then_some(())
        },
    )
    .unwrap_or_else(|error| {
        panic!(
            "{error}; review log: {:?}",
            std::fs::read(session.dir.join("capture-review.log"))
        )
    });
}

fn set_cursor_hidden(session: &mut Session, hidden: bool) {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").unwrap();
    let signature = session
        .hyprland_signature()
        .expect("private capture-test IPC");
    let socket = PathBuf::from(runtime)
        .join("hypr")
        .join(signature)
        .join(".socket.sock");
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(stream, "/keyword cursor:invisible {hidden}").unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert_eq!(response.trim(), "ok");
    session.door().barrier().unwrap();
}

fn diagnostic(session: &mut Session, name: &str) -> Screenshot {
    let path = session.dir.join(format!("{name}.png"));
    let marker = session.dir.join("state/chonkstep/screenshot");
    let pending = marker.with_extension("pending");
    // An empty marker deliberately requests the default screenshot path.
    // Publish the complete filename atomically, never the truncate/write gap.
    std::fs::write(&pending, path.display().to_string()).unwrap();
    std::fs::rename(pending, marker).unwrap();
    poll_until(Duration::from_secs(10), "visible capture chrome", || {
        Screenshot::load(&path).ok()
    })
    .unwrap()
}

fn settle_render_history(session: &mut Session) {
    // Barriers force damage, and diagnostics render a fresh offscreen scene.
    // Drain earlier presents so neither can make a missing input repaint pass.
    let mut quiet_since = std::time::Instant::now();
    poll_until(
        Duration::from_secs(5),
        "capture overlay becomes idle",
        || {
            if session.door().frame_stats().unwrap().render_calls != 0 {
                quiet_since = std::time::Instant::now();
            }
            (quiet_since.elapsed() >= Duration::from_millis(100)).then_some(())
        },
    )
    .unwrap();
}

fn input_schedules_frame(session: &mut Session, what: &str) {
    // Frame-stat reads do not mark scene damage. No barrier, screencopy or
    // diagnostic capture may intervene before this observed real presentation.
    poll_until(Duration::from_secs(5), what, || {
        (session.door().frame_stats().unwrap().render_calls > 0).then_some(())
    })
    .unwrap();
}

fn screenshot_workflow(scale: f32, name: &str) {
    let mut session = boot(name, scale);
    let exports = session.dir.join("exports");
    let probe = profile_binary("chonk-input-probe").unwrap();
    session.launch(probe.to_str().unwrap(), &["1"]).unwrap();
    let window = session.wait_for_window("input-probe").unwrap();
    session
        .door()
        .click((window.x + 50) as f64, (window.y + 50) as f64)
        .unwrap();
    let before = session.screenshot("before").unwrap();
    shortcut(&mut session, 3);
    let path = saved(&exports, 1, "png");
    reviews_opened(&session, &[("xdg-open", &path)]);
    let full = Screenshot::load(&path).unwrap();
    assert_eq!((full.width, full.height), (before.width, before.height));
    assert_eq!(full.pixel(0, 0), before.pixel(0, 0));
    // Clipboard's MIME offer must serve byte-for-byte the published PNG.
    poll_until(Duration::from_secs(10), "PNG clipboard", || {
        let output = Command::new("wl-paste")
            .args(["--no-newline", "--type", "image/png"])
            .env("WAYLAND_DISPLAY", &session.wayland_display)
            .output()
            .ok()?;
        (output.status.success() && output.stdout == std::fs::read(&path).ok()?).then_some(())
    })
    .unwrap();
    if scale == 1.0 {
        session.door().tap_key(99).unwrap();
    } else {
        shortcut(&mut session, 4);
    }
    session
        .door()
        .drag_to((30.0, 40.0), (231.0, 153.0))
        .unwrap();
    let visible = diagnostic(&mut session, "area-overlay");
    assert!(
        visible.diff_fraction(&before, 0) > 0.01,
        "visible capture chrome"
    );
    let exported = session.screenshot("overlay-excluded").unwrap();
    assert_eq!(
        exported.pixel(2, 2),
        before.pixel(2, 2),
        "screencopy excludes dimming"
    );
    session.door().button("left", false).unwrap();
    session.door().barrier().unwrap();
    let area_path = saved(&exports, 2, "png");
    reviews_opened(&session, &[("xdg-open", &path), ("xdg-open", &area_path)]);
    let area = Screenshot::load(&area_path).unwrap();
    assert_eq!(
        (area.width, area.height),
        (201, 113),
        "device pixels, no scale multiplier"
    );
    for y in 0..area.height {
        for x in 0..area.width {
            assert_eq!(
                area.pixel(x, y),
                exported.pixel(x + 30, y + 40),
                "pixel {x},{y}"
            );
        }
    }
    // A window screenshot includes its titlebar and has no surrounding desktop.
    shortcut(&mut session, 4);
    session.door().tap_key(57).unwrap(); // Space -> window
    session
        .door()
        .motion((window.x + 20) as f64, (window.y + 20) as f64)
        .unwrap();
    session.door().barrier().unwrap();
    diagnostic(&mut session, "window-overlay");
    let world = session.world().unwrap();
    let frame = world.frame_of(window.id).unwrap();
    let expected = (frame.w, frame.h);
    session
        .door()
        .click((window.x + 20) as f64, (window.y + 20) as f64)
        .unwrap();
    let window_path = saved(&exports, 3, "png");
    reviews_opened(
        &session,
        &[
            ("xdg-open", &path),
            ("xdg-open", &area_path),
            ("xdg-open", &window_path),
        ],
    );
    let shot = Screenshot::load(&window_path).unwrap();
    assert_eq!((shot.width, shot.height), expected);
    // Escape during a held drag must consume its release and restore typing.
    shortcut(&mut session, 4);
    session
        .door()
        .drag_to((50.0, 50.0), (200.0, 150.0))
        .unwrap();
    session.door().tap_key(1).unwrap();
    session.door().button("left", false).unwrap();
    session.door().barrier().unwrap();
    session.door().tap_key(30).unwrap();
    poll_until(
        Duration::from_secs(5),
        "typing restored after capture cancellation",
        || {
            session
                .client_log("chonk-input-probe")
                .contains("keyboard key 30 down")
                .then_some(())
        },
    )
    .unwrap();
    assert_eq!(files(&exports, "png").len(), 3, "cancel creates no file");
    // Toolbar entry, numbered mode and cancellation exercise the other shortcut.
    shortcut(&mut session, 5);
    diagnostic(&mut session, "capture-toolbar");
    session.door().tap_key(1).unwrap();
}

#[test]
#[ignore = "needs nested Wayland; scripts/e2e.sh --headless --test capture_tool"]
fn screenshots_1x() {
    screenshot_workflow(1.0, "capture-1x");
}
#[test]
#[ignore = "needs nested Wayland; scripts/e2e.sh --headless --test capture_tool"]
fn screenshots_fractional() {
    screenshot_workflow(1.5, "capture-15x");
}
#[test]
#[ignore = "needs nested Wayland; scripts/e2e.sh --headless --test capture_tool"]
fn screenshots_2x() {
    screenshot_workflow(2.0, "capture-2x");
}

fn assert_arrow_at(visible: &Screenshot, elsewhere: &Screenshot, x: u32, y: u32, scale: f32) {
    let width = (12.0 * scale).ceil() as u32;
    let height = (21.0 * scale).ceil() as u32;
    let mut halo = 0;
    let mut body = 0;
    for py in y..y + height {
        for px in x..x + width {
            let here = visible.pixel(px, py);
            let before = elsewhere.pixel(px, py);
            if (0..3).any(|channel| here[channel].abs_diff(before[channel]) > 12) {
                halo += usize::from(here[..3].iter().all(|channel| *channel > 220));
                body += usize::from(here[..3].iter().all(|channel| *channel < 32));
            }
        }
    }
    assert!(
        halo > 12 && body > 12,
        "arrow at ({x}, {y}) must have a bright halo and dark body ABOVE the controls; \
         halo={halo}, body={body}, scale={scale}, visible={}, elsewhere={}",
        visible.path.display(),
        elsewhere.path.display()
    );
    assert!(
        visible.pixel(x, y)[..3]
            .iter()
            .all(|channel| *channel > 220),
        "the arrow's visible tip must follow the pointer hotspot at ({x}, {y})"
    );
}

fn toolbar_cursor_workflow(scale: f32, name: &str) {
    let mut session = boot(name, scale);
    let world = session.world().unwrap();
    let before = session.screenshot("desktop-before-toolbar").unwrap();
    // Capture temporarily owns its cursor even while an Omarchy screensaver
    // owns the desktop's hidden-cursor state. Dismissal must preserve that state.
    set_cursor_hidden(&mut session, true);
    shortcut(&mut session, 5);
    let ui_scale = scale.clamp(1.0, 3.0);
    let width = (720.0 * ui_scale).min((world.output_w - 16) as f32) as u32;
    let height = (86.0 * ui_scale) as u32;
    let left = (world.output_w - width) / 2;
    let top = world.output_h - (24.0 * ui_scale) as u32 - height;
    for index in [0, 3, 6] {
        let x = left + width * index / 7 + (12.0 * ui_scale) as u32;
        let y = top + (10.0 * ui_scale) as u32;
        let other_x = x + (48.0 * ui_scale) as u32;
        // Both positions are inside the SAME control: the hover highlight and
        // every label are identical, leaving only cursor pixels as a difference.
        session.door().motion(other_x as f64, y as f64).unwrap();
        session.door().barrier().unwrap();
        let elsewhere = diagnostic(&mut session, &format!("toolbar-cursor-{index}-right"));
        settle_render_history(&mut session);
        session.door().motion(x as f64, y as f64).unwrap();
        input_schedules_frame(
            &mut session,
            "motion within one capture control presents the cursor",
        );
        let visible = diagnostic(&mut session, &format!("toolbar-cursor-{index}-left"));
        assert_arrow_at(&visible, &elsewhere, x, y, scale);
        assert_arrow_at(&elsewhere, &visible, other_x, y, scale);
        let exported = session
            .screenshot(&format!("toolbar-cursor-{index}-excluded"))
            .unwrap();
        for py in top..top + height {
            for px in left..left + width {
                assert_eq!(
                    exported.pixel(px, py), before.pixel(px, py),
                    "screencopy must exclude both toolbar and cursor at ({px}, {py}), scale={scale}"
                );
            }
        }
    }
    session.door().tap_key(1).unwrap();
    session.door().motion(60.0, 60.0).unwrap();
    session.door().barrier().unwrap();
    let dismissed = diagnostic(&mut session, "toolbar-dismissed-cursor-still-hidden");
    for y in 60..104 {
        for x in 60..84 {
            assert_eq!(
                dismissed.pixel(x, y),
                before.pixel(x, y),
                "dismissal must restore the desktop's hidden cursor ownership"
            );
        }
    }
    set_cursor_hidden(&mut session, false);
    let restored = diagnostic(&mut session, "desktop-cursor-restored");
    assert_arrow_at(&restored, &dismissed, 60, 60, scale);
}

#[test]
#[ignore = "needs nested Wayland; scripts/e2e.sh --headless --test capture_tool"]
fn toolbar_cursor_1x() {
    toolbar_cursor_workflow(1.0, "capture-cursor-1x");
}

#[test]
#[ignore = "needs nested Wayland; scripts/e2e.sh --headless --test capture_tool"]
fn toolbar_cursor_fractional() {
    toolbar_cursor_workflow(1.5, "capture-cursor-15x");
}

#[test]
#[ignore = "needs nested Wayland; scripts/e2e.sh --headless --test capture_tool"]
fn toolbar_cursor_2x() {
    toolbar_cursor_workflow(2.0, "capture-cursor-2x");
}

#[test]
#[ignore = "needs nested Wayland, wf-recorder and ffmpeg"]
fn recording_odd_area_is_playable_and_controls_are_excluded() {
    let mut session = boot("capture-record", 1.5);
    let exports = session.dir.join("exports");
    let before = session.screenshot("before-recording").unwrap();
    shortcut(&mut session, 5);
    session.door().tap_key(6).unwrap(); // 5 -> record area
    session
        .door()
        .drag_to((430.0, 10.0), (731.0, 211.0))
        .unwrap();
    session.door().button("left", false).unwrap();
    session.door().tap_key(28).unwrap(); // Enter starts recording
    diagnostic(&mut session, "recording-indicator");
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        session.door().motion(100.0, 100.0).unwrap();
        session.door().barrier().unwrap();
        std::thread::sleep(Duration::from_millis(40));
    }
    shortcut(&mut session, 5); // Stop / finish
    let path = saved(&exports, 1, "mp4");
    reviews_opened(&session, &[("omacut", &path)]);
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,width,height,pix_fmt:format=duration",
            "-of",
            "json",
        ])
        .arg(&path)
        .output()
        .unwrap();
    assert!(probe.status.success());
    let info: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
    assert_eq!(info["streams"][0]["codec_name"], "h264");
    // Full-range 4:2:0 is reported as yuvj420p by some ffmpeg versions.
    assert!(["yuv420p", "yuvj420p"].contains(&info["streams"][0]["pix_fmt"].as_str().unwrap()));
    assert_eq!(info["streams"][0]["width"], 302);
    assert_eq!(info["streams"][0]["height"], 202);
    assert!(
        info["format"]["duration"]
            .as_str()
            .unwrap()
            .parse::<f64>()
            .unwrap()
            > 1.0
    );
    let frame_path = session.dir.join("recording-first-frame.png");
    assert!(Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(&path)
        .args(["-frames:v", "1"])
        .arg(&frame_path)
        .status()
        .unwrap()
        .success());
    let frame = Screenshot::load(&frame_path).unwrap();
    // This point is underneath the visible recording badge, not merely outside
    // the selected region. Its original desktop pixels must survive in video.
    let expected = before.pixel(500, 25);
    let actual = frame.pixel(70, 15);
    for channel in 0..3 {
        assert!(
            actual[channel].abs_diff(expected[channel]) < 12,
            "no dim overlay encoded: {actual:?} vs {expected:?}"
        );
    }
    shortcut(&mut session, 4);
    session.door().tap_key(1).unwrap(); // Tool can be reopened after finalization.
}

#[test]
#[ignore = "needs nested Wayland; scripts/e2e.sh --headless --test capture_tool"]
fn window_capture_excludes_occluding_windows() {
    let mut session = boot("capture-occluded", 1.5);
    let exports = session.dir.join("exports");
    let probe = profile_binary("chonk-input-probe").unwrap();
    session.launch(probe.to_str().unwrap(), &["1"]).unwrap();
    let target = session.wait_for_window("input-probe").unwrap();
    let baseline = session.screenshot("unoccluded").unwrap();
    // Select the target, then map an occluder: a window screenshot must render
    // the selected window itself, not simply crop the composite desktop.
    shortcut(&mut session, 4);
    session.door().tap_key(57).unwrap();
    session
        .door()
        .motion((target.x + 20) as f64, (target.y + 20) as f64)
        .unwrap();
    session.door().barrier().unwrap();
    session
        .launch(
            "foot",
            &[
                "--title=capture-occluder",
                "--window-size-pixels=800x600",
                "--override",
                "locked-title=yes",
            ],
        )
        .unwrap();
    let occluder = session.wait_for_window("capture-occluder").unwrap();
    let composite = session.screenshot("occluded").unwrap();
    let world = session.world().unwrap();
    let frame = world.frame_of(target.id).unwrap();
    let x = (target.x + 10).max(occluder.x + 10) as u32;
    let y = (target.y + 10).max(occluder.y + 10) as u32;
    assert!(
        x < (target.x + target.w as i32) as u32 && y < (target.y + target.h as i32) as u32,
        "fixture windows overlap"
    );
    assert_ne!(
        composite.pixel(x, y),
        baseline.pixel(x, y),
        "occluder really covers the target"
    );
    let source = (frame.x, frame.y, frame.w, frame.h);
    session.door().tap_key(28).unwrap();
    let image = Screenshot::load(&saved(&exports, 1, "png")).unwrap();
    assert_eq!((image.width, image.height), (source.2, source.3));
    assert_eq!(
        image.pixel((x as i32 - source.0) as u32, (y as i32 - source.1) as u32),
        baseline.pixel(x, y)
    );
}

#[test]
#[ignore = "needs nested Wayland; scripts/e2e.sh --headless --test capture_tool"]
fn saving_failure_does_not_leave_input_grabbed() {
    let mut session = boot("capture-save-failure", 1.0);
    // A file where a directory is expected deterministically fails even under
    // root-run CI; chmod-based permission tests do not.
    std::fs::write(session.dir.join("exports"), "not a directory").unwrap();
    shortcut(&mut session, 4);
    session
        .door()
        .drag_to((30.0, 40.0), (231.0, 153.0))
        .unwrap();
    session.door().button("left", false).unwrap();
    session.door().barrier().unwrap();
    poll_until(Duration::from_secs(10), "failure surfaced", || {
        session.log().contains("Screenshot failed").then_some(())
    })
    .unwrap();
    shortcut(&mut session, 5);
    diagnostic(&mut session, "toolbar-after-failure");
    session.door().tap_key(1).unwrap();
    assert_eq!(
        std::fs::read_to_string(session.dir.join("exports")).unwrap(),
        "not a directory"
    );
    assert!(
        !session.dir.join("capture-review.log").exists(),
        "a failed save must never launch a review app"
    );
}

#[test]
#[ignore = "needs nested Wayland; scripts/e2e.sh --headless --test capture_tool"]
fn retained_area_moves_resizes_and_blocks_desktop_gestures() {
    let mut session = boot("capture-adjust", 1.0);
    shortcut(&mut session, 5);
    session
        .door()
        .drag_to((30.0, 40.0), (231.0, 153.0))
        .unwrap();
    session.door().button("left", false).unwrap();
    session.door().barrier().unwrap();
    session
        .door()
        .drag_to((100.0, 100.0), (110.0, 120.0))
        .unwrap();
    session.door().button("left", false).unwrap();
    session.door().barrier().unwrap();
    session
        .door()
        .drag_to((241.0, 173.0), (261.0, 193.0))
        .unwrap();
    session.door().button("left", false).unwrap();
    session.door().barrier().unwrap();
    let before_nudge = diagnostic(&mut session, "selection-before-keyboard-nudge");
    for key in [106, 108] {
        // Right, then Down: one physical pixel each.
        settle_render_history(&mut session);
        session.door().key(key, true).unwrap();
        session.door().key(key, false).unwrap();
        input_schedules_frame(
            &mut session,
            "arrow-key selection translation presents without pointer motion",
        );
    }
    let after_nudge = diagnostic(&mut session, "selection-after-keyboard-nudge");
    let changed_edge_pixels = (58..68)
        .flat_map(|y| (38..48).map(move |x| (x, y)))
        .filter(|&(x, y)| before_nudge.pixel(x, y) != after_nudge.pixel(x, y))
        .count();
    assert!(changed_edge_pixels > 4,
        "arrow keys must visibly move the selection edge with a stationary pointer and unchanged dimensions");
    let workspace = session.world().unwrap().current_workspace;
    session.door().swipe_begin(3).unwrap();
    for _ in 0..8 {
        session.door().swipe_update(-100.0, 0.0).unwrap();
    }
    session.door().swipe_end(false).unwrap();
    session.door().touch_down(0, 400.0, 300.0).unwrap();
    session.door().touch_up(0).unwrap();
    session.door().touch_frame().unwrap();
    session.door().barrier().unwrap();
    let world = session.world().unwrap();
    assert_eq!(world.current_workspace, workspace);
    diagnostic(&mut session, "adjusted-selection");
    let baseline = session.screenshot("without-chrome").unwrap();
    let width = 720.min(world.output_w - 16);
    let x = (world.output_w - width) / 2 + width * 13 / 14;
    session
        .door()
        .click(x as f64, (world.output_h - 83) as f64)
        .unwrap();
    let image = Screenshot::load(&saved(&session.dir.join("exports"), 1, "png")).unwrap();
    assert_eq!((image.width, image.height), (221, 133));
    assert_eq!(image.pixel(0, 0), baseline.pixel(41, 61));
    assert_eq!(image.pixel(220, 132), baseline.pixel(261, 193));
}

#[test]
#[ignore = "needs nested Wayland, wf-recorder and ffmpeg"]
fn toolbar_screen_modes_work_without_moving_off_the_toolbar_and_badge_stops() {
    let mut session = boot("capture-display", 1.0);
    let probe = profile_binary("chonk-input-probe").unwrap();
    session.launch(probe.to_str().unwrap(), &["1"]).unwrap();
    let window = session.wait_for_window("input-probe").unwrap();
    let world = session.world().unwrap();
    let exports = session.dir.join("exports");
    shortcut(&mut session, 5);
    session
        .door()
        .motion((world.output_w / 2) as f64, (world.output_h - 83) as f64)
        .unwrap();
    session.door().tap_key(2).unwrap(); // 1 -> screenshot display
    session.door().tap_key(28).unwrap();
    let image = Screenshot::load(&saved(&exports, 1, "png")).unwrap();
    assert_eq!(
        (image.width, image.height),
        (world.output_w, world.output_h)
    );
    shortcut(&mut session, 5);
    session.door().tap_key(5).unwrap(); // 4 -> record display
    session.door().tap_key(28).unwrap();
    poll_until(Duration::from_secs(10), "recorder creates data", || {
        std::fs::read_dir(&exports)
            .ok()?
            .flatten()
            .any(|e| {
                e.path().extension().is_some_and(|x| x == "mkv")
                    && e.metadata().is_ok_and(|m| m.len() > 1000)
            })
            .then_some(())
    })
    .unwrap();
    // A press owned by an application must return its release to that
    // application even when the drag ends above the recording's Stop badge.
    let releases = session
        .client_log("chonk-input-probe")
        .matches(" release ")
        .count();
    session
        .door()
        .drag_to(
            ((window.x + 50) as f64, (window.y + 60) as f64),
            ((world.output_w / 2) as f64, 30.0),
        )
        .unwrap();
    session.door().button("left", false).unwrap();
    session.door().barrier().unwrap();
    poll_until(
        Duration::from_secs(5),
        "application receives release over recording badge",
        || {
            (session
                .client_log("chonk-input-probe")
                .matches(" release ")
                .count()
                == releases + 1)
                .then_some(())
        },
    )
    .unwrap();
    assert!(
        files(&exports, "mp4").is_empty(),
        "an application drag release must not stop recording"
    );
    session
        .door()
        .click((world.output_w / 2) as f64, 30.0)
        .unwrap();
    let path = saved(&exports, 1, "mp4");
    let screenshot_path = files(&exports, "png").pop().unwrap();
    reviews_opened(
        &session,
        &[("xdg-open", &screenshot_path), ("omacut", &path)],
    );
    assert!(Command::new("ffprobe")
        .args(["-v", "error"])
        .arg(path)
        .status()
        .unwrap()
        .success());
}
