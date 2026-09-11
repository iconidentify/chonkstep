//! Density changes must invalidate physical-size echoes from older configures.
use chonk_testkit::{poll_until, profile_binary, Session, SessionOptions};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::Duration,
};

fn resize(s: &mut Session, w: u32, h: u32) {
    let socket = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap())
        .join("hypr")
        .join(s.hyprland_signature().unwrap())
        .join(".socket.sock");
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
        .write_all(format!("/dispatch resizeactive exact {w} {h}").as_bytes())
        .unwrap();
    let mut reply = String::new();
    stream.take(1024).read_to_string(&mut reply).unwrap();
    assert_eq!(reply.trim(), "ok");
    poll_until(Duration::from_secs(10), "client answers resize", || {
        let world = s.world().ok()?;
        let client = world.window_matching("scale-change-probe")?;
        (client.w == w && client.h == h && client.presented_w == w && client.presented_h == h)
            .then_some(())
    })
    .unwrap();
}

fn change_density(marker: &Path, stage: &str) {
    std::fs::write(marker, stage).unwrap();
    poll_until(
        Duration::from_secs(10),
        "client committed the requested density transition",
        || {
            (std::fs::read_to_string(marker.with_extension("done"))
                .ok()?
                .as_str()
                == stage)
                .then_some(())
        },
    )
    .unwrap();
}

#[test]
#[ignore = "requires nested Wayland"]
fn mapped_buffer_and_viewport_density_changes_keep_pixels_inside_the_frame() {
    for style in wm_theme::SUPPORTED_DECORATION_STYLES {
    for scale in [1.0, 1.5, 2.0] {
        for mode in ["buffer", "viewport", "viewport-destination"] {
            let mut s = Session::boot(
                &format!("scale-change-{}-{scale}-{mode}", style.name()),
                SessionOptions {
                    scale: Some(scale),
                    config_extra: format!("show_dock = false\nomarchy_menu = false\nhyprland_config = false\ndecoration_style = {:?}\n", style.name()),
                    env: vec![("RUST_LOG".into(), "info,wm_wayland::xdg=trace".into())],
                    ..Default::default()
                },
            )
            .unwrap();
            let act = s.dir.join("density");
            let binary = profile_binary("chonk-scale-change-probe").unwrap();
            s.launch_isolated(binary.to_str().unwrap(), &[act.to_str().unwrap(), mode])
                .unwrap();
            s.wait_for_window("scale-change-probe").unwrap();
            resize(&mut s, 300, 160);
            resize(&mut s, 400, 200);
            resize(&mut s, 200, 100);
            change_density(&act, "2");
            // The fractional integer-buffer fallback is intentionally 1.5x;
            // both declarations still use the same effective output factor.
            let (w, h) = match (mode, scale == 1.5) {
                ("viewport-destination", true) => (150, 75),
                ("viewport-destination", false) => (200, 100),
                (_, true) => (300, 150),
                (_, false) => (400, 200),
            };
            poll_until(
                Duration::from_secs(10),
                "frame follows new committed density",
                || {
                    let world = s.world().ok()?;
                    let c = world.window_matching("scale-change-probe")?;
                    (c.w == w && c.h == h && c.presented_w == w && c.presented_h == h).then_some(())
                },
            )
            .unwrap_or_else(|e| panic!("{e}: {scale}/{mode}\n{}", s.log()));
            assert_pixels(&mut s, w, h);
            // Changing back also must not be suppressed by prior physical asks.
            change_density(&act, "1");
            poll_until(Duration::from_secs(10), "density restored", || {
                let world = s.world().ok()?;
                let c = world.window_matching("scale-change-probe")?;
                (c.w == 200 && c.h == 100 && c.presented_w == 200).then_some(())
            })
            .unwrap();
            change_density(&act, "2");
            poll_until(Duration::from_secs(10), "second density increase", || {
                let world = s.world().ok()?;
                let c = world.window_matching("scale-change-probe")?;
                (c.w == w && c.presented_w == w).then_some(())
            })
            .unwrap();
            let next_scale = if scale == 1.5 { "2" } else { "1.5" };
            s.launch(
                "wlr-randr",
                &["--output", "chonkstep", "--scale", next_scale],
            )
            .unwrap();
            poll_until(Duration::from_secs(10), "output scale applied", || {
                s.client_status("wlr-randr")
                    .ok()
                    .flatten()
                    .map(|status| assert!(status.success()))
            })
            .unwrap();
            change_density(&act, "2 ");
            let changed = poll_until(
                Duration::from_secs(10),
                "density settles after output scale change",
                || {
                    let world = s.world().ok()?;
                    let c = world.window_matching("scale-change-probe")?;
                    (c.w == c.presented_w && c.h == c.presented_h).then_some((c.w, c.h))
                },
            )
            .unwrap();
            assert_pixels(&mut s, changed.0, changed.1);
        }
    }
    }
}

fn assert_pixels(s: &mut Session, w: u32, h: u32) {
    let world = s.world().unwrap();
    let c = world.window_matching("scale-change-probe").unwrap();
    let f = world.frames.iter().find(|f| f.window == c.id).unwrap();
    assert!(f.w >= c.w && f.w - c.w < 30);
    assert!(f.h > c.h && f.h - c.h < 100);
    let image = s.screenshot("density-two").unwrap();
    let green = |p: [u8; 4]| p[0] < 40 && p[1] > 215 && p[2] < 65;
    let mut bounds = (u32::MAX, u32::MAX, 0, 0);
    for y in 0..image.height {
        for x in 0..image.width {
            if green(image.pixel(x, y)) {
                bounds = (
                    bounds.0.min(x),
                    bounds.1.min(y),
                    bounds.2.max(x),
                    bounds.3.max(y),
                );
            }
        }
    }
    assert_eq!(
        bounds,
        (
            c.x as u32,
            c.y as u32,
            c.x as u32 + w - 1,
            c.y as u32 + h - 1
        ),
        "actual painted pixels disagree with geometry"
    );
    // The theme's top band must actually reach the new right edge.
    let bar_y = (f.y + (c.y - f.y) / 2) as u32;
    let visual_right = f.x + f.w as i32 - f.input_margin as i32;
    let near_right = image.pixel((visual_right - 8) as u32, bar_y);
    let outside = image.pixel((visual_right + 8) as u32, bar_y);
    assert_ne!(near_right, outside, "titlebar stopped short of its frame");
}
