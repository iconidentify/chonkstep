//! Opt-in captures must include the displayed controls without contaminating
//! simultaneous clean captures or losing the protocol's independent cursor bit.
use super::*;
use std::process::{Child, ExitStatus};

fn finish(child: &mut Child, timeout: Duration) -> ExitStatus {
    match poll_until(timeout, "capture process exits", || {
        child.try_wait().ok().flatten()
    }) {
        Ok(status) => status,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{error}");
        }
    }
}

fn grim(session: &Session, path: &Path, demo: bool, cursor: bool) -> Child {
    let display = format!(
        "{}{}",
        if demo { "chonkstep-capture-" } else { "" },
        session.wayland_display
    );
    let mut cmd = Command::new("grim");
    if cursor {
        cmd.arg("-c");
    }
    cmd.arg(path)
        .env("WAYLAND_DISPLAY", display)
        .env_remove("WAYLAND_SOCKET")
        .spawn()
        .unwrap()
}

fn shot(session: &Session, label: &str, demo: bool, cursor: bool) -> Screenshot {
    let path = session.dir.join(format!("{label}.png"));
    assert!(finish(
        &mut grim(session, &path, demo, cursor),
        Duration::from_secs(15)
    )
    .success());
    Screenshot::load(&path).unwrap()
}

fn open_area(session: &mut Session) {
    shortcut(session, 5);
    session.door().tap_key(4).unwrap();
    session
        .door()
        .drag_to((80.0, 75.0), (340.0, 235.0))
        .unwrap();
    session.door().button("left", false).unwrap();
    session.door().barrier().unwrap();
}

fn mixed_workflow(scale: f32, name: &str, heads: bool) {
    let mut session = boot(name, scale);
    if heads {
        session.door().virtual_outputs(true).unwrap();
    }
    let clean = session.screenshot("clean-before").unwrap();
    open_area(&mut session);
    let expected = diagnostic(&mut session, "displayed-controls");
    assert!(expected.diff_fraction(&clean, 0) > 0.03);
    let visible = shot(&session, "demo-cursor", true, true);
    assert_eq!(
        visible.diff_fraction(&expected, 0),
        0.0,
        "demo stream matches the displayed UI"
    );
    let no_cursor = shot(&session, "demo-no-cursor", true, false);
    assert!(
        visible.diff_fraction(&no_cursor, 0) > 0.0,
        "the real selection cursor is recorded only when requested"
    );
    for y in 0..visible.height {
        for x in 0..visible.width {
            if !(300..380).contains(&x) || !(195..275).contains(&y) {
                assert_eq!(
                    visible.pixel(x, y),
                    no_cursor.pixel(x, y),
                    "cursor policy changed non-cursor pixels at {x},{y}"
                );
            }
        }
    }
    // Submit both policies together and reverse request order on each cycle.
    // A shared download/cache keyed only by geometry would leak one to the other.
    for cycle in 0..4 {
        let plain_path = session.dir.join(format!("mixed-{cycle}-clean.png"));
        let demo_path = session.dir.join(format!("mixed-{cycle}-demo.png"));
        let (mut first, mut second) = if cycle % 2 == 0 {
            (
                grim(&session, &plain_path, false, false),
                grim(&session, &demo_path, true, true),
            )
        } else {
            (
                grim(&session, &demo_path, true, true),
                grim(&session, &plain_path, false, false),
            )
        };
        assert!(finish(&mut first, Duration::from_secs(15)).success());
        assert!(finish(&mut second, Duration::from_secs(15)).success());
        assert_eq!(
            Screenshot::load(&plain_path)
                .unwrap()
                .diff_fraction(&clean, 0),
            0.0
        );
        assert_eq!(
            Screenshot::load(&demo_path)
                .unwrap()
                .diff_fraction(&expected, 0),
            0.0
        );
    }
    session.door().tap_key(1).unwrap();
    session.door().barrier().unwrap();
    let after = shot(&session, "demo-dismissed", true, false);
    assert_eq!(
        after.diff_fraction(&clean, 0),
        0.0,
        "dismissal removes all cached capture controls"
    );
    assert!(session.compositor_alive());
}

#[test]
#[ignore = "needs nested Wayland, grim"]
fn demo_controls_1x() {
    mixed_workflow(1.0, "demo-controls-1x", false);
}

#[test]
#[ignore = "needs nested Wayland, grim"]
fn demo_controls_fractional() {
    mixed_workflow(1.5, "demo-controls-15x", false);
}

#[test]
#[ignore = "needs nested Wayland, grim"]
fn demo_controls_2x() {
    mixed_workflow(2.0, "demo-controls-2x", false);
}

#[test]
#[ignore = "needs nested Wayland, grim"]
fn demo_controls_two_outputs() {
    mixed_workflow(1.0, "demo-controls-heads", true);
}

struct Recorder {
    display: String,
    directory: PathBuf,
    active: bool,
}
impl Recorder {
    fn command(&self) -> Command {
        let mut c =
            Command::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/chonkrec"));
        c.env("WAYLAND_DISPLAY", &self.display)
            .env_remove("WAYLAND_SOCKET")
            .env("CHONKREC_DIR", &self.directory);
        let fixture = self.directory.join("join-fixture-bin");
        if fixture.is_dir() {
            let mut paths = vec![fixture];
            paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
            c.env("PATH", std::env::join_paths(paths).unwrap());
        }
        c
    }
    fn stop(&mut self) {
        let status = finish(
            &mut self.command().args(["stop", "--no-open"]).spawn().unwrap(),
            Duration::from_secs(70),
        );
        self.active = false;
        assert!(status.success());
    }
}
impl Drop for Recorder {
    fn drop(&mut self) {
        if self.active {
            if let Ok(mut child) = self.command().args(["stop", "--no-open"]).spawn() {
                let _ = poll_until(Duration::from_secs(70), "stop failed demo", || {
                    child.try_wait().ok().flatten()
                });
            }
        }
    }
}

#[test]
#[ignore = "needs nested Wayland, real chonkrec, wf-recorder and ffmpeg"]
fn chonkrec_demo_encodes_controls_and_preserves_clean_exports() {
    let mut session = boot("chonkrec-demo-video", 1.5);
    let clean = session.screenshot("video-clean").unwrap();
    open_area(&mut session);
    let expected = diagnostic(&mut session, "video-visible");
    let path = session.dir.join("Demo take with spaces.mp4");
    let mut recorder = Recorder {
        display: session.wayland_display.clone(),
        directory: session.dir.clone(),
        active: true,
    };
    assert!(finish(
        &mut recorder
            .command()
            .args(["start", "--demo", "--no-open", "-r", "30", "-o"])
            .arg(&path)
            .spawn()
            .unwrap(),
        Duration::from_secs(20)
    )
    .success());
    // A concurrent plain capture must still be clean while chonkrec is active.
    assert_eq!(
        shot(&session, "during-demo-clean", false, false).diff_fraction(&clean, 0),
        0.0
    );
    std::thread::sleep(Duration::from_millis(2200));
    // Plain captures request a presentation, advancing the recorder after the hold.
    shot(&session, "during-demo-visible", true, true);
    std::thread::sleep(Duration::from_millis(300));
    recorder.stop();
    drop(recorder);
    let frame_path = session.dir.join("encoded-demo.png");
    assert!(Command::new("ffmpeg")
        .args(["-v", "error", "-xerror", "-i"])
        .arg(&path)
        .args(["-f", "null", "-"])
        .status()
        .unwrap()
        .success());
    assert!(Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&path)
        .args(["-frames:v", "1"])
        .arg(&frame_path)
        .status()
        .unwrap()
        .success());
    let frame = Screenshot::load(&frame_path).unwrap();
    assert_keyframe_spacing(&path);
    assert!(
        frame.diff_fraction(&expected, 20) < 0.015,
        "encoded demo matches the actual displayed controls"
    );
    assert!(
        frame.diff_fraction(&clean, 20) > 0.03,
        "recording contains the overlay, not a clean export"
    );
    session.door().tap_key(1).unwrap();
    session.door().barrier().unwrap();
    assert_eq!(
        session
            .screenshot("after-chonkrec")
            .unwrap()
            .diff_fraction(&clean, 0),
        0.0
    );
}

#[test]
#[ignore = "needs nested Wayland, real chonkrec and ffmpeg"]
fn chonkrec_start_flushes_low_bitrate_video_before_reporting_success() {
    let mut session = boot("chonkrec-small-fragments", 1.0);
    let probe = chonk_testkit::profile_binary("chonk-fullscreen-probe").unwrap();
    session.launch_isolated(probe.to_str().unwrap(), &[]).unwrap();
    session.wait_for_window("chonk-fullscreen-probe").unwrap();
    session.door().tap_key(33).unwrap();
    poll_until(Duration::from_secs(10), "flat fullscreen client", || {
        let world = session.world().ok()?;
        let window = world.window_matching("chonk-fullscreen-probe")?;
        (window.presented_w == world.output_w && window.presented_h == world.output_h).then_some(())
    }).unwrap();
    let path = session.dir.join("small-fragments.mp4");
    let mut recorder = Recorder {
        display: session.wayland_display.clone(), directory: session.dir.clone(), active: true,
    };
    assert!(finish(&mut recorder.command()
        .args(["start", "--no-open", "-r", "1", "-o"]).arg(&path).spawn().unwrap(),
        Duration::from_secs(20)).success());
    // Read the still-growing file: startup promises actual video, not merely
    // an MP4 header. At one fps these small fragments must be flushed promptly.
    let runtime = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap());
    let work = PathBuf::from(std::fs::read_to_string(runtime.join("chonkrec.work")).unwrap());
    let packets = Command::new("ffprobe").args(["-v", "error", "-select_streams", "v:0",
        "-read_intervals", "%+#1", "-show_entries", "packet=pts_time", "-of", "csv=p=0"])
        .arg(work.join("seg-000.mp4")).output().unwrap();
    assert!(packets.status.success() && !packets.stdout.is_empty());
    std::thread::sleep(Duration::from_secs(3));
    recorder.stop();
    assert_keyframe_spacing(&path);
}

#[test]
#[ignore = "needs a real nested compositor restart, chonkrec and ffmpeg"]
fn chonkrec_demo_survives_a_compositor_restart() {
    restart_workflow(false);
}

#[test]
#[ignore = "needs nested Wayland, real chonkrec and ffmpeg"]
fn chonkrec_recovery_reencode_preserves_one_second_keyframes() {
    restart_workflow(true);
}

fn restart_workflow(force_reencode: bool) {
    let mut session = boot(if force_reencode { "chonkrec-demo-reencode" } else { "chonkrec-demo-restart" }, 1.0);
    if force_reencode {
        use std::os::unix::fs::PermissionsExt;
        let bin = session.dir.join("join-fixture-bin");
        std::fs::create_dir(&bin).unwrap();
        let wrapper = bin.join("ffmpeg");
        std::fs::write(&wrapper, r#"#!/bin/sh
case "$*" in
  *"-f concat"*"-c copy"*) printf forced > "${0%/*}/fallback-used"; exit 42 ;;
esac
exec /usr/bin/ffmpeg "$@"
"#).unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    open_area(&mut session);
    let before = diagnostic(&mut session, "restart-visible-before");
    let path = session.dir.join("restarted-demo.mp4");
    let mut recorder = Recorder {
        display: session.wayland_display.clone(),
        directory: session.dir.clone(),
        active: true,
    };
    assert!(finish(
        &mut recorder
            .command()
            .args(["start", "--demo", "--no-open", "-o"])
            .arg(&path)
            .spawn()
            .unwrap(),
        Duration::from_secs(20)
    )
    .success());
    std::thread::sleep(Duration::from_millis(1200));
    let pid = session.compositor_pid();
    let runtime = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap());
    let work = PathBuf::from(std::fs::read_to_string(runtime.join("chonkrec.work")).unwrap());
    session.restart().unwrap();
    assert_eq!(session.compositor_pid(), pid);
    assert_eq!(session.wayland_display, recorder.display);
    open_area(&mut session);
    let after = diagnostic(&mut session, "restart-visible-after");
    assert_eq!(after.diff_fraction(&before, 0), 0.0);
    poll_until(Duration::from_secs(25), "second recording segment", || {
        let segments = files(&work, "mp4");
        (segments.len() >= 2 && segments.last()?.metadata().ok()?.len() > 0).then_some(())
    })
    .unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    recorder.stop();
    if force_reencode {
        assert!(session.dir.join("join-fixture-bin/fallback-used").exists());
    }
    assert_keyframe_spacing(&path);
    assert!(Command::new("ffmpeg")
        .args(["-v", "error", "-xerror", "-i"])
        .arg(&path)
        .args(["-f", "null", "-"])
        .status()
        .unwrap()
        .success());
    for (label, tail) in [("head", false), ("tail", true)] {
        let frame = session.dir.join(format!("restart-{label}.png"));
        let mut ffmpeg = Command::new("ffmpeg");
        ffmpeg.args(["-v", "error"]);
        if tail {
            ffmpeg.args(["-sseof", "-0.25"]);
        }
        assert!(ffmpeg
            .arg("-i")
            .arg(&path)
            .args(["-frames:v", "1"])
            .arg(&frame)
            .status()
            .unwrap()
            .success());
        assert!(
            Screenshot::load(&frame).unwrap().diff_fraction(&after, 20) < 0.015,
            "{label} segment retains the displayed capture overlay"
        );
    }
    assert!(session.compositor_alive());
}
