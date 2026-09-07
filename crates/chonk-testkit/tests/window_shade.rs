//! Check the pixels below a rolled-up frame, including its one-pixel side
//! borders. A correct frame rectangle alone cannot prove sparse chrome fits.

use chonk_testkit::{keys, profile_binary, FrameInfo, Screenshot, Session, SessionOptions};

fn toggle_shade(session: &mut Session) {
    let door = session.door();
    door.key(keys::LEFTALT, true).unwrap();
    door.key(keys::LEFTSHIFT, true).unwrap();
    door.tap_key(31).unwrap(); // S: the default alt+shift+s binding.
    door.key(keys::LEFTSHIFT, false).unwrap();
    door.key(keys::LEFTALT, false).unwrap();
    door.barrier().unwrap();
}

fn frame(session: &mut Session, window: u64) -> FrameInfo {
    session
        .world()
        .unwrap()
        .frames
        .into_iter()
        .find(|frame| frame.window == window)
        .unwrap()
}

fn assert_body_cleared(
    before: &Screenshot,
    after: &Screenshot,
    full: &FrameInfo,
    shade_height: u32,
) {
    let left = u32::try_from(full.x).unwrap();
    let top = u32::try_from(full.y).unwrap();
    assert!(left + full.w <= after.width && top + full.h <= after.height);
    // Sample every pixel, including the outermost columns. An area average
    // or a centre sample hides the two narrow lines this regression leaves.
    for y in top + shade_height..top + full.h {
        for x in left..left + full.w {
            assert_eq!(
                after.pixel(x, y),
                before.pixel(x, y),
                "chrome/content remains below the shade at ({x}, {y}); capture: {}",
                after.path.display()
            );
        }
    }
}

#[test]
#[ignore = "needs a nested Wayland session: scripts/e2e.sh --headless --test window_shade"]
fn shade_clears_both_borders_and_stays_clear_after_repaint_and_drag() {
    for scale in [1.0, 1.5, 2.0] {
        let mut session = Session::boot(
            &format!("window-shade-{scale}"),
            SessionOptions {
                scale: Some(scale),
                config_extra: "show_dock = false\n".into(),
                ..Default::default()
            },
        )
        .unwrap();
        session.door().barrier().unwrap();
        let desktop = session.screenshot("empty-desktop").unwrap();
        let probe = profile_binary("chonk-fullscreen-probe").unwrap();
        session
            .launch(probe.to_str().unwrap(), &["ShadeProbe", "shade-probe"])
            .unwrap();
        let window = session.wait_for_window("shade-probe").unwrap();
        session.door().barrier().unwrap();
        let full = frame(&mut session, window.id);
        let expanded = session.screenshot("expanded").unwrap();
        // Establish that both strips really were painted before hiding them.
        for x in [full.x as u32, full.x as u32 + full.w - 1] {
            let y = window.y as u32 + window.h / 2;
            assert_ne!(
                expanded.pixel(x, y),
                desktop.pixel(x, y),
                "the test must expose each border"
            );
        }

        for cycle in 0..2 {
            toggle_shade(&mut session);
            let shaded = frame(&mut session, window.id);
            assert!(shaded.mapped && shaded.h < full.h);
            assert!(
                !session
                    .world()
                    .unwrap()
                    .windows
                    .iter()
                    .find(|w| w.id == window.id)
                    .unwrap()
                    .mapped
            );
            let shot = session.screenshot(&format!("shaded-{cycle}")).unwrap();
            assert_body_cleared(&desktop, &shot, &full, shaded.h);
            toggle_shade(&mut session);
            let restored = frame(&mut session, window.id);
            assert_eq!(
                (restored.x, restored.y, restored.w, restored.h),
                (full.x, full.y, full.w, full.h)
            );
            let shot = session.screenshot(&format!("restored-{cycle}")).unwrap();
            assert_eq!(
                shot.diff_fraction(&expanded, 0),
                0.0,
                "unshading restores the original pixels"
            );
        }

        let title = (
            full.x as f64 + full.w as f64 / 2.0,
            (full.y + window.y) as f64 / 2.0,
        );
        session.door().click(title.0, title.1).unwrap();
        session.door().click(title.0, title.1).unwrap();
        let shaded = frame(&mut session, window.id);
        assert!(shaded.h < full.h, "titlebar double-click shades");
        let shot = session.screenshot("double-click-shaded").unwrap();
        assert_body_cleared(&desktop, &shot, &full, shaded.h);

        // Focusing a real second client forces an inactive-titlebar repaint.
        // Move it out of the former body's way before checking those pixels.
        session
            .launch(
                probe.to_str().unwrap(),
                &["FocusNeighbour", "focus-neighbour"],
            )
            .unwrap();
        let neighbour = session.wait_for_window("focus-neighbour").unwrap();
        let neighbour_frame = frame(&mut session, neighbour.id);
        let world = session.world().unwrap();
        assert_eq!(world.logical_focus, Some(neighbour.id));
        let neighbour_title = (
            neighbour_frame.x as f64 + neighbour_frame.w as f64 / 2.0,
            (neighbour_frame.y + neighbour.y) as f64 / 2.0,
        );
        let delta = (
            world.output_w as f64 - neighbour_frame.w as f64 - 40.0 - neighbour_frame.x as f64,
            world.output_h as f64 - neighbour_frame.h as f64 - 40.0 - neighbour_frame.y as f64,
        );
        session
            .door()
            .drag_to(
                neighbour_title,
                (neighbour_title.0 + delta.0, neighbour_title.1 + delta.1),
            )
            .unwrap();
        session.door().button("left", false).unwrap();
        session.door().barrier().unwrap();
        let shot = session.screenshot("unfocused-shade").unwrap();
        assert_body_cleared(&desktop, &shot, &full, shaded.h);
        // Dragging the shade refocuses and repaints it again.
        let from = (title.0 + 60.0, title.1);
        session
            .door()
            .drag_to(from, (from.0 + 80.0, from.1 + 60.0))
            .unwrap();
        session.door().button("left", false).unwrap();
        session.door().barrier().unwrap();
        let moved = frame(&mut session, window.id);
        assert_eq!(moved.h, shaded.h, "dragging keeps the window rolled up");
        assert_ne!((moved.x, moved.y), (full.x, full.y));
        let shot = session.screenshot("dragged-shade").unwrap();
        let moved_full = FrameInfo { h: full.h, ..moved };
        assert_body_cleared(&desktop, &shot, &moved_full, shaded.h);
        toggle_shade(&mut session);
        let restored = frame(&mut session, window.id);
        assert_eq!((restored.w, restored.h), (full.w, full.h));
        assert!(
            session
                .world()
                .unwrap()
                .windows
                .iter()
                .find(|w| w.id == window.id)
                .unwrap()
                .mapped
        );
    }
}
