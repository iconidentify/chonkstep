//! The end-to-end suite: every test here is a regression the user
//! actually hit, shipped by a change whose unit tests were green, and
//! found by hand — named for the behavior it pins, not the code it
//! touches. Each one boots a real nested compositor (winit backend,
//! isolated `XDG_*`), launches real clients, injects input through
//! the compositor's test door, and asserts on the ledger and on
//! screenshots taken through the compositor's own screencopy.
//!
//! # Running
//!
//! These need a live Wayland session to nest inside, which GitHub's
//! headless runners do not have (ci.yml's wayland job documents why a
//! compositor cannot boot there at all), so they are `#[ignore]`d.
//! Locally, inside any Wayland session:
//!
//! ```text
//! scripts/e2e.sh
//! # or by hand:
//! cargo build -p chonkstep-wayland
//! cargo test -p chonk-testkit -- --ignored --test-threads=1
//! ```
//!
//! `--test-threads=1` because each test opens a compositor window on
//! the developer's desktop; five at once fight for focus and make the
//! host tile them into shapes nobody asserted on. Artifacts (logs,
//! screenshots) land under `$TMPDIR/chonk-testkit/<test-name>/` and
//! are left there for post-mortems.
//!
//! # No sleeps
//!
//! Every wait is a bounded poll on an observable condition: the door's
//! `barrier` (input dispatched + frame rendered), a window appearing
//! in or leaving the ledger, a geometry becoming true. The only
//! `sleep` anywhere is the poll cadence inside `poll_until`.

use std::time::Duration;

use chonk_testkit::{is_dark, poll_until, Session, SessionOptions, WindowInfo};

/// A generous deadline for conditions that involve a client acting on
/// events (crossing a drag threshold, re-committing at a new scale).
const ACT: Duration = Duration::from_secs(10);

/// Launches a zenity question dialog and waits for it to map. The
/// standard CSD guinea pig: GTK draws its own titlebar (chonkstep must
/// not frame it — see the `negotiated_decoration` story in
/// `wm-wayland/src/state.rs`), and its header drag exercises the
/// client-initiated move path the drag regression lived in.
fn launch_question(session: &mut Session, title: &str) -> WindowInfo {
    session
        .launch("zenity", &["--question", "--title", title, "--text", "Click OK"])
        .expect("zenity should launch");
    session.wait_for_window(title).expect("the zenity dialog should map")
}

/// The regression: a drag whose release was delivered to the client
/// while `wm-core` never heard the drag was over, leaving the window
/// glued to the cursor and following every later motion. Fixed by
/// `DragEnded` (see `on_pointer_button` in `wm-wayland/src/input.rs`);
/// this test is that fix as an executable statement.
///
/// The drag is the client-initiated kind on purpose — press on a CSD
/// dialog's own titlebar, GTK asks for `xdg_toplevel.move` — because
/// that is the exact shape that shipped broken: the release's routing
/// target is the client's content, which is precisely the case where
/// the compositor must end the drag *itself*.
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn drag_ends_when_the_button_comes_up() {
    let mut session = Session::boot("drag-ends", SessionOptions::default()).unwrap();
    let window = launch_question(&mut session, "TestDrag");
    let start = (window.x, window.y);

    // Press in the CSD headerbar (top strip of the client's own
    // content — there is no server frame on a GTK dialog) and drag.
    let grip = (window.x as f64 + window.w as f64 / 2.0, window.y as f64 + 20.0);
    let target = (grip.0 + 240.0, grip.1 + 180.0);
    session.door().drag_to(grip, target).unwrap();

    // The window followed the drag — otherwise this test would pass
    // vacuously against a compositor that ignores drags entirely.
    let moved = {
        let door = session.door();
        poll_until(ACT, "the dialog to follow the drag", || {
            let world = door.windows().ok()?;
            let now = world.window_matching("TestDrag")?;
            ((now.x, now.y) != start).then_some((now.x, now.y))
        })
        .unwrap()
    };

    // Release. The drag is over the instant the button is up; the
    // barrier guarantees the release has been routed and a frame
    // drawn before we take the reference state.
    session.door().button("left", false).unwrap();
    session.door().barrier().unwrap();
    let at_release = {
        let world = session.world().unwrap();
        let now = world.window_matching("TestDrag").unwrap();
        (now.x, now.y)
    };
    let before = session.screenshot("after-release").unwrap();

    // Post-release motion, big and in several settled steps: a window
    // still glued to the cursor follows every one of them.
    for (x, y) in [(target.0 + 300.0, target.1 + 200.0), (target.0 + 600.0, target.1 + 400.0)] {
        session.door().motion(x, y).unwrap();
        session.door().barrier().unwrap();
    }
    let world = session.world().unwrap();
    let now = world.window_matching("TestDrag").unwrap();
    assert_eq!(
        (now.x, now.y),
        at_release,
        "the window kept following the pointer after the button came up \
         (it had moved to {moved:?} during the drag, was at {at_release:?} at release)"
    );

    // Pixel corroboration: nothing but the pointer sprite may have
    // changed between the release and the post-release motions.
    let after = session.screenshot("after-post-release-motion").unwrap();
    let changed = before.diff_fraction(&after, 12);
    assert!(
        changed < 0.01,
        "{:.3}% of the screen changed after the release — a moved cursor is a fraction of \
         a percent, a moved window is not (screenshots: {} vs {})",
        changed * 100.0,
        before.path.display(),
        after.path.display()
    );
}

/// The regression: clicks landing offset from where they visually
/// landed, because the drag anchor came from a stale pointer position
/// (see `WaylandBackend::pointer`'s doc comment). The observable
/// contract is simpler and stronger than any anchor internals: a
/// click injected at a button's on-screen position must press that
/// button. Here the button is the Yes of a zenity question, whose
/// position is computed from the ledger rectangle the door reports —
/// if routing, hit-testing, or the ledger's idea of where the window
/// is disagree with the pixels, the dialog stays open and this fails.
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn click_lands_where_it_visually_lands() {
    let mut session = Session::boot("click-lands", SessionOptions::default()).unwrap();
    let window = launch_question(&mut session, "TestClick");
    session.screenshot("dialog-open").unwrap();

    // The Yes button sits in the lower-right action row of a GTK
    // question dialog; proportions of the window rect keep this
    // stable across font sizes. (Verified against the rendered
    // dialog: Yes spans roughly 0.55–0.9 of the width at 0.75–0.85 of
    // the height.)
    let x = window.x as f64 + window.w as f64 * 0.72;
    let y = window.y as f64 + window.h as f64 * 0.79;
    session.door().click(x, y).unwrap();

    // The observable meaning of "the click pressed Yes": the dialog
    // closes. A click that missed hits the dialog body (nothing) or
    // No (also closes — accepted: either way it landed inside the
    // 47px-wide button row it aimed at; the offset bug was a whole
    // window-width of error).
    session.wait_for_window_gone("TestClick").expect(
        "the dialog should have closed — the injected click did not land on the button \
         that is visibly at that position",
    );
    session.screenshot("dialog-closed").unwrap();
}

/// The regression: booting (or reloading) at scale 2 collapsed the
/// desktop into a quarter of the screen — wallpaper clipped to its
/// top-left quarter, dock multiplied off the screen entirely (the
/// full account lives above `advertise_scale` in
/// `wm-wayland/src/state.rs`). The assertions are that bug's three
/// symptoms, inverted: wallpaper reaches all four corners, the dock's
/// ledger rectangle has dock-colored pixels actually in it, and a
/// client renders at twice its scale-1 width (told scale 2 through
/// the outputs, it commits 2x buffers into a 2x physical rectangle).
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn scale_2_composition_stays_intact() {
    // Reference run at scale 1: how wide is the same dialog?
    let width_at_1 = {
        let mut session =
            Session::boot("scale-ref", SessionOptions { scale: Some(1.0), ..SessionOptions::default() }).unwrap();
        let window = launch_question(&mut session, "TestScale");
        window.w
    };

    let mut session = Session::boot("scale-2", SessionOptions { scale: Some(2.0), ..SessionOptions::default() }).unwrap();
    let window = launch_question(&mut session, "TestScale");
    session.door().barrier().unwrap();
    let world = session.world().unwrap();
    assert_eq!(world.scale, 2.0, "the session should be running at the configured scale");

    let shot = session.screenshot("scale2-desktop").unwrap();

    // Wallpaper covers the full output: 16x16 mean at each corner,
    // none of them black. In the shipped bug everything right of and
    // below the half-way lines was unpainted.
    let (w, h) = (shot.width, shot.height);
    for (name, x, y) in
        [("top-left", 2, 2), ("top-right", w - 18, 2), ("bottom-left", 2, h - 18), ("bottom-right", w - 18, h - 18)]
    {
        let mean = shot.mean_rgb(x, y, 16, 16);
        assert!(
            !is_dark(mean),
            "the {name} corner is black ({mean:?}) — the desktop is not covering the output \
             (screenshot: {})",
            shot.path.display()
        );
    }

    // The dock column: the ledger says where it is; the pixels must
    // agree. Sample the middle of the column and compare with the
    // wallpaper just left of it — the dock's chiseled tiles are
    // nothing like the wallpaper, and in the shipped bug this
    // rectangle held bare wallpaper while the dock itself was drawn
    // off-screen.
    let dock = world.dock().expect("a dock column at the right edge of the ledger").clone();
    let inside = shot.mean_rgb(dock.x as u32 + dock.w / 4, dock.y as u32 + dock.w / 2, dock.w / 2, 16);
    let beside =
        shot.mean_rgb((dock.x - dock.w as i32) as u32, dock.y as u32 + dock.w / 2, dock.w / 2, 16);
    let contrast = inside
        .iter()
        .zip(beside.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f64, f64::max);
    assert!(
        contrast > 25.0,
        "the dock's ledger rectangle {dock:?} holds wallpaper-looking pixels \
         (inside {inside:?} vs beside {beside:?}) — the dock is not being drawn where the \
         ledger says it is (screenshot: {})",
        shot.path.display()
    );

    // The client is crisp 2x, not stretched 1x: its physical width is
    // twice the scale-1 run's, within a small tolerance for theme
    // rounding. A blurry upscale would keep the 1x logical size too —
    // this is the "GTK actually heard scale 2" assertion.
    let ratio = window.w as f64 / width_at_1 as f64;
    assert!(
        (ratio - 2.0).abs() < 0.2,
        "the dialog rendered {}px wide at scale 2 vs {width_at_1}px at scale 1 (ratio {ratio:.2}, \
         wanted ~2.0) — clients are not being told the scale",
        window.w
    );
}

/// The frameless half of interactive resize: a client that draws its
/// own chrome has no server-side resize borders, so grabbing its edge
/// means the pointer is over the client's *shadow* — outside the
/// ledger's content rectangle — and the whole path only works if the
/// hit test extends a frameless window's claim into that margin (see
/// `frameless_claims` in `wm-wayland/src/input.rs`) and the
/// client-initiated `xdg_toplevel.resize` then drives `wm-core`'s
/// resize machinery. A resizable zenity (--text-info) is the guinea
/// pig; the corner drag must make it bigger.
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn frameless_resize_works() {
    let mut session = Session::boot("frameless-resize", SessionOptions::default()).unwrap();
    let text = session.dir.join("resize-me.txt");
    std::fs::write(&text, "resize me\n").unwrap();
    session
        .launch("zenity", &["--text-info", "--title", "TestResize", "--filename", text.to_str().unwrap()])
        .unwrap();
    let window = session.wait_for_window("TestResize").unwrap();
    session.screenshot("before-resize").unwrap();

    // 2px outside the bottom-right corner of the content rect: on the
    // GTK shadow, where its resize grip lives — and where a hit test
    // that stops at the content rect would see only the desktop.
    let grip = (window.x as f64 + window.w as f64 + 2.0, window.y as f64 + window.h as f64 + 2.0);
    session.door().drag_to(grip, (grip.0 + 120.0, grip.1 + 120.0)).unwrap();

    // Grown yet? The client acks configures asynchronously, so poll
    // the ledger rather than reading it once.
    {
        let door = session.door();
        poll_until(ACT, "the window to grow past its original size", || {
            let world = door.windows().ok()?;
            let now = world.window_matching("TestResize")?;
            (now.w > window.w && now.h > window.h).then_some(())
        })
        .expect("the corner drag should have resized the frameless window");
    }
    session.door().button("left", false).unwrap();
    session.door().barrier().unwrap();
    session.screenshot("after-resize").unwrap();
}

/// The live-reload gesture (`scripts/reload.sh`: rewrite the config,
/// touch the marker) must re-scale the running session's chrome in
/// place — without the compositor dying, which is what a reload
/// regression costs on Wayland: every client dies with the socket.
/// This drives the same marker file the script touches, in the
/// isolated state dir, and asserts the observable outcome: the ledger
/// reports the new scale, the dock column doubles, the process lives,
/// and the composition stays intact (the scale-2 collapse arrived
/// through exactly this reload path once).
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn live_reload_applies() {
    let mut session = Session::boot("live-reload", SessionOptions { scale: Some(1.0), ..SessionOptions::default() }).unwrap();
    session.door().barrier().unwrap();
    let before = session.world().unwrap();
    assert_eq!(before.scale, 1.0);
    let dock_before = before.dock().expect("a dock at scale 1").clone();

    session.rewrite_config("scale = 2\n").unwrap();
    session.request_reload().unwrap();

    // The restyle is slow in a debug build (~11s measured), so the
    // deadline is generous; the condition is still observable, not a
    // sleep: the ledger's scale and the dock's doubled width.
    {
        let door = session.door();
        poll_until(
            Duration::from_secs(30),
            "the reload to re-scale the dock in place",
            || {
                let world = door.windows().ok()?;
                let dock = world.dock()?;
                (world.scale == 2.0 && dock.w == dock_before.w * 2).then_some(())
            },
        )
        .expect("the live reload never applied");
    }
    assert!(session.compositor_alive(), "the reload killed the compositor");

    // And the rescaled composition is whole — the collapse regression
    // check, after a *live* rescale rather than a boot.
    session.door().barrier().unwrap();
    let shot = session.screenshot("after-live-reload").unwrap();
    let (w, h) = (shot.width, shot.height);
    for (name, x, y) in
        [("top-left", 2, 2), ("top-right", w - 18, 2), ("bottom-left", 2, h - 18), ("bottom-right", w - 18, h - 18)]
    {
        let mean = shot.mean_rgb(x, y, 16, 16);
        assert!(
            !is_dark(mean),
            "after the live rescale the {name} corner is black ({mean:?}) — the composition \
             collapsed (screenshot: {})",
            shot.path.display()
        );
    }
}

/// Counts lines in a client's `WAYLAND_DEBUG=1` stream that mention
/// `object` receiving `event` — e.g. (`"wl_keyboard#"`, `".enter("`).
/// The client's own protocol log is the same ground truth the
/// restore-input bug below was diagnosed from: the ledger can say
/// "focused" all it wants, only the wire says what the client was
/// told.
fn wire_events(log: &std::path::Path, object: &str, event: &str) -> usize {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains(object) && line.contains(event))
        .count()
}

/// The regression: restoring a miniaturized client-decorated window
/// left it input-dead until the user clicked another window and came
/// back. Miniaturize set `wm-core`'s focus to `None` but the seat
/// kept keyboard focus on the hidden surface and its toplevel kept
/// `Activated`, so the restore — refocusing the *same* window —
/// deduplicated into nothing: no `wl_keyboard.enter`, no configure.
/// A client that had minimized itself (`xdg_toplevel.set_minimized`,
/// Edge's own Minimize menu item) was still waiting for exactly that
/// configure to learn it was unminimized, and until one arrived it
/// discarded every key and click the seat kept delivering.
///
/// The invariant this pins, from the client's own WAYLAND_DEBUG
/// stream: hiding the focused window carries a real
/// `wl_keyboard.leave` to it, and restoring it a fresh
/// `wl_keyboard.enter` — the cycle is visible on the wire, never a
/// dedup. (`publish_active_window(None)` → `FocusIntent::Nothing` in
/// `wm-wayland`; the zenity guinea pig is CSD and frameless-managed,
/// the same shape as Edge.)
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn restore_after_miniaturize_is_a_real_focus_cycle() {
    let mut session = Session::boot("miniaturize-restore-focus", SessionOptions::default()).unwrap();
    session
        .launch("env", &["WAYLAND_DEBUG=1", "zenity", "--question", "--title", "TestMini", "--text", "hide me"])
        .expect("zenity should launch");
    let window = session.wait_for_window("TestMini").expect("the zenity dialog should map");
    let log = session.dir.join("client-0-env.log");

    // Focus it with a click in its content and wait until the client
    // itself has seen keyboard focus at least once.
    session
        .door()
        .click(window.x as f64 + window.w as f64 / 2.0, window.y as f64 + window.h as f64 / 3.0)
        .unwrap();
    poll_until(ACT, "the client to see a wl_keyboard.enter", || {
        (wire_events(&log, "wl_keyboard#", ".enter(") >= 1).then_some(())
    })
    .expect("the focus click never reached the client's keyboard");

    let enters_before = wire_events(&log, "wl_keyboard#", ".enter(");
    let leaves_before = wire_events(&log, "wl_keyboard#", ".leave(");
    let shells_before: Vec<u64> =
        session.door().windows().unwrap().shells.iter().map(|s| s.id).collect();

    // Miniaturize via the default alt+shift+m chord. Two modifiers,
    // so the raw key path rather than `Door::chord` (which holds one).
    {
        let door = session.door();
        door.key(56, true).unwrap(); // KEY_LEFTALT
        door.key(42, true).unwrap(); // KEY_LEFTSHIFT
        door.tap_key(50).unwrap(); // KEY_M
        door.key(42, false).unwrap();
        door.key(56, false).unwrap();
        door.barrier().unwrap();
    }

    // The hide must be real to the client: ledger unmapped AND a
    // keyboard leave on its wire.
    {
        let door = session.door();
        poll_until(ACT, "the ledger to show the window hidden", || {
            let world = door.windows().ok()?;
            let now = world.windows.iter().find(|w| w.title.contains("TestMini"))?;
            (!now.mapped).then_some(())
        })
        .expect("the miniaturize chord never hid the window");
    }
    poll_until(ACT, "the client to see a wl_keyboard.leave", || {
        (wire_events(&log, "wl_keyboard#", ".leave(") > leaves_before).then_some(())
    })
    .expect(
        "the hidden window never got a wl_keyboard.leave — the seat is still parked on it, \
         and the restore will dedup into an input-dead window",
    );

    // Restore via the icon tile: the one mapped shell surface that
    // appeared with the miniaturize.
    let tile = session
        .door()
        .windows()
        .unwrap()
        .shells
        .into_iter()
        .find(|s| s.mapped && !shells_before.contains(&s.id))
        .expect("miniaturizing should have grown an icon tile shell");
    session
        .door()
        .click(tile.x as f64 + tile.w as f64 / 2.0, tile.y as f64 + tile.h as f64 / 2.0)
        .unwrap();

    {
        let door = session.door();
        poll_until(ACT, "the ledger to show the window restored", || {
            let world = door.windows().ok()?;
            world.window_matching("TestMini").map(|_| ())
        })
        .expect("clicking the icon tile never restored the window");
    }
    // And the client was told: a fresh enter, not a dedup.
    poll_until(ACT, "the client to see a fresh wl_keyboard.enter", || {
        (wire_events(&log, "wl_keyboard#", ".enter(") > enters_before).then_some(())
    })
    .expect(
        "the restored window never got a fresh wl_keyboard.enter — restore was a dedup, \
         and a self-minimized client stays input-dead",
    );
}

/// The inbound half of EWMH on the Wayland stack: an X11 pager's
/// `_NET_CURRENT_DESKTOP` ClientMessage to the XWayland root must
/// actually switch the workspace. Before `xewmh.rs` grew its inbound
/// drain, publishing was one-way — a pager could read every property
/// and change nothing, because the control messages (sent to the root
/// with the SubstructureRedirect|SubstructureNotify mask) landed on a
/// connection that never looked.
///
/// This test *is* a minimal pager: it connects to the nested
/// session's XWayland display with x11rb, sends the message the spec
/// prescribes, and asserts the round trip — the compositor switches
/// (growing the workspace row on demand, same as the keybinding) and
/// republishes `_NET_CURRENT_DESKTOP` with the new index, which is
/// exactly what a real pager reads to move its highlight.
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn an_x11_pager_can_switch_the_workspace() {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{
        AtomEnum, ClientMessageEvent, ConnectionExt as _, EventMask, CLIENT_MESSAGE_EVENT,
    };

    let session = Session::boot("ewmh-inbound-desktop", SessionOptions::default()).unwrap();

    // XWayland starts with the session; its display number is
    // announced in the log once the server is ready. tracing writes
    // ANSI color escapes between the key and the value (the same trap
    // `Session::boot` documents for the socket line), so the escapes
    // are stripped before parsing.
    let display = poll_until(Duration::from_secs(30), "XWayland to announce its display", || {
        let log = session.log();
        let line = log.lines().find(|line| line.contains("XWayland ready"))?;
        let plain: String = {
            let mut out = String::new();
            let mut chars = line.chars();
            while let Some(c) = chars.next() {
                if c == '\u{1b}' {
                    // Skip to the terminating 'm' of the CSI sequence.
                    for e in chars.by_ref() {
                        if e == 'm' {
                            break;
                        }
                    }
                } else {
                    out.push(c);
                }
            }
            out
        };
        plain.split("display=").nth(1)?.trim().parse::<u32>().ok()
    })
    .expect("the nested session never brought XWayland up");

    let (conn, screen_num) =
        x11rb::rust_connection::RustConnection::connect(Some(&format!(":{display}")))
            .expect("connecting to the nested XWayland display");
    let root = conn.setup().roots[screen_num].root;
    let net_current_desktop = conn
        .intern_atom(false, b"_NET_CURRENT_DESKTOP")
        .unwrap()
        .reply()
        .unwrap()
        .atom;

    // The spec's gesture, verbatim: a ClientMessage to the root,
    // data[0] = the desktop index, sent with the redirect|notify mask.
    let message = ClientMessageEvent::new(32, root, net_current_desktop, [1, 0, 0, 0, 0]);
    conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        message,
    )
    .unwrap();
    conn.flush().unwrap();
    assert_eq!(message.response_type, CLIENT_MESSAGE_EVENT);

    // The observable round trip: the compositor heard the message,
    // switched, and republished the property a pager highlights from.
    poll_until(ACT, "_NET_CURRENT_DESKTOP to read back as 1", || {
        let reply = conn
            .get_property(false, root, net_current_desktop, AtomEnum::CARDINAL, 0, 1)
            .ok()?
            .reply()
            .ok()?;
        let value = reply.value32()?.next()?;
        (value == 1).then_some(())
    })
    .expect("the pager's desktop-switch message never took effect");
}
