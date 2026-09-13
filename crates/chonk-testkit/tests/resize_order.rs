//! Resize replies must converge without gaps, overflow or configure feedback.

use chonk_testkit::{poll_until, profile_binary, Session, SessionOptions};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

#[test]
#[ignore = "needs nested Wayland"]
fn a_terminal_cell_snap_must_not_leave_an_oversized_frame() {
    for (theme, scale) in [
        ("system-7-classic", 1.0),
        ("nextstep-classic", 1.0),
        ("obsidian", 1.5),
        ("washi", 2.0),
    ] {
        let mut session = Session::boot(
            &format!("resize-terminal-{theme}-{scale}"),
            SessionOptions {
                scale: Some(scale),
                config_extra: format!("show_dock=false\nomarchy_menu=false\nhyprland_config=false\ntheme='{theme}'\ndecoration_style='auto'\n"),
                env: vec![
                    ("CHONKSTEP_HYPRLAND_IPC".into(), "1".into()),
                    ("RUST_LOG".into(), "info,wm_wayland::xdg=trace,wm_wayland::backend_impl=trace".into()),
                ],
                ..Default::default()
            },
        ).unwrap();
        session
            .launch(
                "foot",
                &[
                    "--title=resize-cells",
                    "--override=locked-title=yes",
                    "--override=resize-by-cells=yes",
                    "--override=colors.background=ffffff",
                    "--override=colors.foreground=000000",
                    "--override=font=monospace:size=10",
                    "sh",
                ],
            )
            .unwrap();
        let window = session.wait_for_window("resize-cells").unwrap();
        session
            .door()
            .click((window.x + 30) as f64, (window.y + 30) as f64)
            .unwrap();
        let socket = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap())
            .join("hypr")
            .join(session.hyprland_signature().unwrap())
            .join(".socket.sock");
        for delta in [1, 2, 3, 8, 20, -1, -2, 31, -17] {
            let mut ipc = UnixStream::connect(&socket).unwrap();
            ipc.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            ipc.write_all(format!("/dispatch resizeactive 0 {delta}").as_bytes())
                .unwrap();
            let mut reply = String::new();
            ipc.read_to_string(&mut reply).unwrap();
            assert_eq!(reply, "ok");
            poll_until(
                Duration::from_secs(3),
                "frame fits the terminal's committed cell grid",
                || {
                    let world = session.world().ok()?;
                    let current = world.windows.iter().find(|w| w.id == window.id)?;
                    (current.w == current.presented_w && current.h == current.presented_h)
                        .then_some(())
                },
            )
            .unwrap_or_else(|error| {
                session.screenshot("mismatched-frame").unwrap();
                panic!(
                    "{error}; delta={delta}; {:?}\n{}",
                    session.world().unwrap().windows,
                    session.log()
                );
            });
        }
    }
}

#[test]
#[ignore = "needs a nested session: scripts/e2e.sh --headless --release"]
fn an_old_rendered_commit_cannot_erase_a_staged_ipc_resize() {
    const PROBE: &str = "chonk-resize-order-probe";
    const EVENT: Duration = Duration::from_secs(10);
    for round in 0..16 {
        let theme = ["system-7-classic", "nextstep-classic", "obsidian", "washi"][round % 4];
        let mut session = Session::boot(
            &format!("resize-order-{round}"),
            SessionOptions {
                config_extra: format!("show_dock=false\nomarchy_menu=false\nhyprland_config=false\ntheme='{theme}'\ndecoration_style='auto'\n"),
                env: vec![
                    ("CHONKSTEP_HYPRLAND_IPC".into(), "1".into()),
                    (
                        "RUST_LOG".into(),
                        "info,wm_wayland::xdg=trace,wm_wayland::backend_impl=trace".into(),
                    ),
                ],
                ..Default::default()
            },
        )
        .unwrap();
        let act = session.dir.join("resize-act");
        let binary = profile_binary(PROBE).unwrap();
        session
            .launch_isolated(binary.to_str().unwrap(), &[act.to_str().unwrap()])
            .unwrap();
        let original = session.wait_for_window("resize-order-probe").unwrap();
        assert_eq!((original.w, original.h), (400, 300));
        session.door().barrier().unwrap();
        std::fs::write(act, b"resize").unwrap();
        let answer = poll_until(EVENT, "the configure answering the IPC resize", || {
            session
                .client_log(PROBE)
                .lines()
                .find(|line| line.starts_with("configure ") && line.ends_with("requested=true"))
                .map(str::to_owned)
        })
        .unwrap_or_else(|error| {
            panic!("{error}; {}\n{}", session.client_log(PROBE), session.log())
        });
        assert_eq!(
            answer,
            "configure 520 380 requested=true",
            "round {round}\n{}\n{}",
            session.client_log(PROBE),
            session.log()
        );
        // The probe deliberately keeps its old 400x300 buffer. Both growth
        // and shrinkage must present one filled interior, with no exposed black
        // backing or old-size pixels spilling over the frame.
        for (w, h) in [(520, 380), (280, 220)] {
            if w == 280 {
                let socket = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap())
                    .join("hypr")
                    .join(session.hyprland_signature().unwrap())
                    .join(".socket.sock");
                let mut ipc = UnixStream::connect(socket).unwrap();
                ipc.set_read_timeout(Some(EVENT)).unwrap();
                ipc.write_all(b"/dispatch resizeactive exact 280 220")
                    .unwrap();
                let mut reply = String::new();
                ipc.read_to_string(&mut reply).unwrap();
                assert_eq!(reply, "ok");
            }
            session.door().barrier().unwrap();
            let world = session.world().unwrap();
            let current = world.window_matching("resize-order-probe").unwrap();
            assert_eq!((current.w, current.h), (w, h));
            assert_eq!((current.presented_w, current.presented_h), (400, 300));
            let shot = session.screenshot(&format!("pending-{w}x{h}")).unwrap();
            let x = current.x as u32;
            let y = current.y as u32;
            for (px, py) in [
                (x + 4, y + h / 2),
                (x + w - 5, y + h / 2),
                (x + w / 2, y + 4),
                (x + w / 2, y + h - 5),
            ] {
                assert_eq!(
                    shot.pixel(px, py),
                    [144, 144, 144, 255],
                    "{theme} pending {w}x{h} at {px},{py}"
                );
            }
            if w == 280 {
                assert_ne!(
                    shot.pixel(x + 390, y + 100),
                    [144, 144, 144, 255],
                    "the old buffer must not overflow the new frame"
                );
            }
        }
        println!(
            "resize-order round={round}: correct configure and gap-free pending buffers ({theme})"
        );
    }
}
