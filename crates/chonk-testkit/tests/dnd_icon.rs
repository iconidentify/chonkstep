//! The icon a client hands to `wl_data_device.start_drag` — a file
//! thumbnail, a link preview — is drawn under the pointer for the life of
//! the drag, at the offset its commits asked for, and is gone the moment
//! the drag ends. The input probe's `dnd icon` mode carries a solid orange
//! surface committed with an `attach` dx/dy after `start_drag`; the pixels
//! come back through screencopy without the cursor overlay, which is what
//! proves the icon is scene content rather than part of the pointer.

use std::time::Duration;

use chonk_testkit::{poll_until, profile_binary, Screenshot, Session, SessionOptions, WindowInfo};

const PROBE: &str = "chonk-input-probe";
const EVENT: Duration = Duration::from_secs(10);
/// The probe's red content fill and its orange icon fill, as RGB
/// (`chonk-input-probe`'s `ICON_PIXEL` read back through screencopy).
const CONTENT: [u8; 3] = [0xC0, 0x40, 0x40];
const ICON: [u8; 3] = [0xF0, 0xA0, 0x20];
/// The icon's logical size and the `attach` offset of its first commit,
/// mirroring the probe's `ICON_SIZE` and `ICON_OFFSET`.
const ICON_SIZE: (f64, f64) = (40.0, 30.0);
const ICON_OFFSET: (f64, f64) = (6.0, 10.0);
/// Where the drag starts and where it is carried to, in the probe's
/// logical content units: both leave the whole icon inside its 400x300.
const START: (f64, f64) = (80.0, 70.0);
const CARRY: (f64, f64) = (170.0, 100.0);
/// Per-channel slack: grim reads a fractional-scale output through its
/// logical size, blending neighbours into edge pixels, so every sample is
/// taken well inside or well outside the icon.
const TOLERANCE: u8 = 16;

#[derive(Debug)]
struct InputEvent {
    sequence: u64,
    kind: String,
}

fn event_after(session: &Session, after: u64, kinds: &[&str]) -> Option<InputEvent> {
    session.client_log(PROBE).lines().rev().find_map(|line| {
        let mut fields = line.strip_prefix("input ")?.split_whitespace();
        let event = InputEvent { sequence: fields.next()?.parse().ok()?, kind: fields.next()?.into() };
        (event.sequence > after && kinds.contains(&event.kind.as_str())).then_some(event)
    })
}

fn wait_event(session: &Session, after: u64, kinds: &[&str]) -> InputEvent {
    poll_until(EVENT, &format!("the client's next {kinds:?} event"), || event_after(session, after, kinds))
        .unwrap_or_else(|error| panic!("{error}\n{}", session.client_log(PROBE)))
}

fn wait_line(session: &Session, line: &str) {
    poll_until(EVENT, &format!("the probe to report {line:?}"), || session.client_log(PROBE).contains(line).then_some(()))
        .unwrap_or_else(|error| panic!("{error}\n{}", session.client_log(PROBE)));
}

fn close_to(pixel: [u8; 4], rgb: [u8; 3]) -> bool {
    pixel.iter().zip(rgb).all(|(channel, wanted)| channel.abs_diff(wanted) <= TOLERANCE)
}

/// A booted session with the probe mapped in `dnd icon` mode, and the
/// probe's content origin in output pixels.
struct Drag {
    session: Session,
    scale: f64,
    origin: (f64, f64),
}

impl Drag {
    fn boot(name: &str, scale: f32) -> Drag {
        Self::boot_options(name, scale, SessionOptions {
            config_extra: "show_dock = false\n".into(), ..Default::default()
        })
    }

    fn boot_options(name: &str, scale: f32, options: SessionOptions) -> Drag {
        Self::boot_mode(name, scale, options, "dnd")
    }

    fn boot_mode(name: &str, scale: f32, options: SessionOptions, mode: &str) -> Drag {
        let mut session = Session::boot(
            name,
            SessionOptions { scale: Some(scale), ..options },
        )
        .expect("nested compositor");
        let binary = profile_binary(PROBE).expect("input probe built");
        session.launch(binary.to_str().unwrap(), &[&scale.to_string(), mode, "icon"]).expect("probe launches");
        let window: WindowInfo = session.wait_for_window("input-probe").expect("probe maps");
        session.door().barrier().unwrap();
        let origin = (f64::from(window.x - window.offset_x), f64::from(window.y - window.offset_y));
        Drag { session, scale: f64::from(scale), origin }
    }

    /// The output pixel under the probe's logical content point `at`.
    fn global(&self, at: (f64, f64)) -> (i32, i32) {
        ((self.origin.0 + at.0 * self.scale).round() as i32, (self.origin.1 + at.1 * self.scale).round() as i32)
    }

    /// The icon's rectangle in output pixels when the pointer is over `at`:
    /// the pointer plus the committed offset, both converted by the probe's
    /// scale, which is also the icon's.
    fn icon_rect(&self, at: (f64, f64)) -> (i32, i32, i32, i32) {
        let (x, y) = self.global(at);
        (
            x + (ICON_OFFSET.0 * self.scale).round() as i32,
            y + (ICON_OFFSET.1 * self.scale).round() as i32,
            (ICON_SIZE.0 * self.scale).round() as i32,
            (ICON_SIZE.1 * self.scale).round() as i32,
        )
    }

    /// Hovers `at`, presses the right button — the probe's cue to start
    /// its drag — and waits until the icon's first commit is in.
    fn start(&mut self) -> u64 {
        let (x, y) = self.global(START);
        self.session.door().motion(f64::from(x), f64::from(y)).unwrap();
        let last = wait_event(&self.session, 0, &["enter", "motion"]);
        self.session.door().button("right", true).unwrap();
        let last = wait_event(&self.session, last.sequence, &["press"]);
        wait_line(&self.session, "icon committed");
        self.session.door().barrier().unwrap();
        last.sequence
    }

    fn screenshot(&mut self, label: &str) -> Screenshot {
        self.session.door().barrier().unwrap();
        self.session.screenshot(label).unwrap()
    }

    /// Asserts the icon is drawn exactly where a pointer over `at` puts
    /// it: orange well inside its rectangle, the probe's red between the
    /// pointer and the offset corner (so the offset was honoured rather
    /// than the icon sitting on the hotspot), and red again just past its
    /// far corner.
    fn assert_icon_at(&self, shot: &Screenshot, at: (f64, f64)) {
        let (left, top, width, height) = self.icon_rect(at);
        let pointer = self.global(at);
        let pixel = |(x, y): (i32, i32)| shot.pixel(x as u32, y as u32);
        let mut problems = Vec::new();
        for point in [(left + 3, top + 3), (left + width / 2, top + height / 2), (left + width - 4, top + height - 4)] {
            if !close_to(pixel(point), ICON) {
                problems.push(format!("{point:?} is {:?}, not the icon's {ICON:?}", pixel(point)));
            }
        }
        for point in [(pointer.0 + 2, pointer.1 + 2), (left + width + 3, top + height + 3)] {
            if !close_to(pixel(point), CONTENT) {
                problems.push(format!("{point:?} is {:?}, not the content's {CONTENT:?}", pixel(point)));
            }
        }
        assert!(problems.is_empty(), "icon for a pointer at {at:?}: {} ({})", problems.join("; "), shot.path.display());
    }

    /// Asserts nothing of the icon remains where a pointer over `at` drew it.
    fn assert_no_icon_at(&self, shot: &Screenshot, at: (f64, f64), expected: [u8; 3]) {
        let (left, top, width, height) = self.icon_rect(at);
        let point = (left + width / 2, top + height / 2);
        let pixel = shot.pixel(point.0 as u32, point.1 as u32);
        assert!(
            close_to(pixel, expected),
            "{point:?} is {pixel:?}, not {expected:?}: the icon is still drawn ({})",
            shot.path.display()
        );
    }
}

fn icon_follows_the_pointer_at(scale: f32) {
    let mut drag = Drag::boot(&format!("dnd-icon-{scale}"), scale);
    let mut last = drag.start();

    // Over the origin itself the grab reports an enter; the icon is drawn
    // at the pointer plus its committed offset.
    let (x, y) = drag.global(START);
    drag.session.door().motion(f64::from(x), f64::from(y)).unwrap();
    last = wait_event(&drag.session, last, &["dnd-enter"]).sequence;
    let shot = drag.screenshot("icon-at-start");
    drag.assert_icon_at(&shot, START);
    // A visible icon is paced: the frame callback its commit requested is
    // answered from the output under the pointer.
    wait_line(&drag.session, "icon frame done");

    // Carried with the pointer, leaving nothing behind.
    let (x, y) = drag.global(CARRY);
    drag.session.door().motion(f64::from(x), f64::from(y)).unwrap();
    last = wait_event(&drag.session, last, &["dnd-motion"]).sequence;
    let shot = drag.screenshot("icon-carried");
    drag.assert_icon_at(&shot, CARRY);
    drag.assert_no_icon_at(&shot, START, CONTENT);
    // Drawn, never hit: the point under the icon still resolves to the
    // content beneath it, which is what the drop will land on.
    let (left, top, width, height) = drag.icon_rect(CARRY);
    let hit = drag.session.door().hit(left + width / 2, top + height / 2).unwrap();
    assert_eq!(hit, "content", "the drag icon must not intercept the hit-test");

    // The drop removes it.
    drag.session.door().button("right", false).unwrap();
    wait_event(&drag.session, last, &["dnd-drop"]);
    let shot = drag.screenshot("icon-dropped");
    drag.assert_no_icon_at(&shot, CARRY, CONTENT);
}

#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn a_protected_windows_drag_icon_stays_on_screen_but_out_of_captures() {
    let mut drag = Drag::boot_options("dnd-icon-private", 1.0, SessionOptions {
        config_extra: "show_dock = false\nhyprland_config = true\n".into(),
        config_root_files: vec![("hypr/hyprland.conf".into(),
            "windowrule = no_screen_share on, match:class ^input-probe$\n".into())],
        ..Default::default()
    });
    drag.start();
    let (x, y) = drag.global(START);
    drag.session.door().motion(f64::from(x), f64::from(y)).unwrap();
    wait_event(&drag.session, 0, &["dnd-enter"]);
    wait_line(&drag.session, "icon frame done");
    let path = drag.session.dir.join("display-icon.png");
    std::fs::write(drag.session.dir.join("state/chonkstep/screenshot"), path.display().to_string()).unwrap();
    let display = poll_until(EVENT, "display capture", || Screenshot::load(&path).ok()).unwrap();
    let (left, top, width, height) = drag.icon_rect(START);
    assert!(close_to(display.pixel((left + width / 2) as u32, (top + height / 2) as u32), ICON),
        "the private icon must remain visible on screen");
    let capture = drag.screenshot("private-icon");
    drag.assert_no_icon_at(&capture, START, [51, 51, 51]);
    drag.session.door().button("right", false).unwrap();
}

#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn a_touch_drag_uses_its_own_serial_while_a_mouse_button_is_held() {
    let mut drag = Drag::boot_mode("dnd-icon-touch-with-mouse", 1.0, SessionOptions {
        config_extra: "show_dock = false\nhyprland_config = true\n".into(),
        config_root_files: vec![("hypr/hyprland.conf".into(),
            "windowrule = no_screen_share on, match:class ^input-probe$\n".into())],
        ..Default::default()
    }, "dnd-touch");
    let (x, y) = drag.global(START);
    drag.session.door().motion(f64::from(x), f64::from(y)).unwrap();
    wait_event(&drag.session, 0, &["enter", "motion"]);
    drag.session.door().button("left", true).unwrap();
    wait_event(&drag.session, 0, &["press"]);

    let (x, y) = drag.global(CARRY);
    drag.session.door().touch_down(0, f64::from(x), f64::from(y)).unwrap();
    wait_line(&drag.session, "icon committed");
    let carried = (230.0, 170.0);
    let (x, y) = drag.global(carried);
    drag.session.door().touch_motion(0, f64::from(x), f64::from(y)).unwrap();
    wait_event(&drag.session, 0, &["dnd-enter", "dnd-motion"]);
    wait_line(&drag.session, "icon frame done");

    let path = drag.session.dir.join("display-touch-icon.png");
    std::fs::write(drag.session.dir.join("state/chonkstep/screenshot"), path.display().to_string()).unwrap();
    let display = poll_until(EVENT, "display capture", || Screenshot::load(&path).ok()).unwrap();
    drag.assert_icon_at(&display, carried);
    let capture = drag.screenshot("private-touch-icon");
    drag.assert_no_icon_at(&capture, carried, [51, 51, 51]);
    drag.session.door().touch_up(0).unwrap();
    drag.session.door().button("left", false).unwrap();
}

#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn the_drag_icon_follows_the_pointer_and_leaves_on_drop() {
    icon_follows_the_pointer_at(1.0);
}

#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn the_drag_icon_follows_the_pointer_at_integer_scale() {
    icon_follows_the_pointer_at(2.0);
}

#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn the_drag_icon_follows_the_pointer_at_fractional_scale() {
    icon_follows_the_pointer_at(1.5);
}

/// Locking ends the drag in flight and takes its icon with it: the lock
/// screen must never show application pixels, and the client hears its
/// source cancelled rather than being left holding a drag.
#[test]
#[ignore = "needs a live Wayland session to nest inside"]
fn locking_the_session_ends_the_drag_and_removes_its_icon() {
    let mut drag = Drag::boot("dnd-icon-lock", 1.0);
    drag.start();
    let shot = drag.screenshot("icon-before-lock");
    drag.assert_icon_at(&shot, START);

    // `--hold` keeps the lock rather than cycling through unlocks, so the
    // screenshot below is of a locked session and not of a transition.
    let locker = profile_binary("chonk-lock-probe").expect("lock probe built");
    drag.session.launch(locker.to_str().unwrap(), &["--hold"]).expect("the locker launches");
    poll_until(Duration::from_secs(15), "the locker to report the session locked", || {
        drag.session.client_log("chonk-lock-probe").contains("holding the lock").then_some(())
    })
    .unwrap_or_else(|error| panic!("{error}\n{}", drag.session.client_log("chonk-lock-probe")));
    // The drag never reached a target, so ending it cancels the source.
    wait_line(&drag.session, "drag cancelled");
    let shot = drag.screenshot("icon-locked");
    let (left, top, width, height) = drag.icon_rect(START);
    let pixel = shot.pixel((left + width / 2) as u32, (top + height / 2) as u32);
    assert!(!close_to(pixel, ICON), "the icon is still drawn over the lock screen ({})", shot.path.display());
}
