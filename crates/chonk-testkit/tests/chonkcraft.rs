//! Opt-in real-game input checks. Supply CHONKSTEP_CHONKCRAFT_JAVA, _JAR and
//! _PACK explicitly. The jar and licensed assets are external, read-only inputs.
//! No game actions are injected: the Java wrapper only observes AWT delivery
//! and painted menu state after real input crosses the nested compositor.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chonk_testkit::{keys, poll_until, Session, SessionOptions, WindowInfo};
use serde_json::Value;

const EVENT: Duration = Duration::from_secs(20);
const JAVA_MOUSE_PRESSED: u64 = 501;
const JAVA_MOUSE_RELEASED: u64 = 502;
const KEY_F11: u32 = 87;

struct GameFiles {
    java: PathBuf,
    jar: PathBuf,
    pack: PathBuf,
}

impl GameFiles {
    fn requested() -> Option<Self> {
        let java = std::env::var_os("CHONKSTEP_CHONKCRAFT_JAVA")?;
        let canonical = |name: &str, path: PathBuf| {
            let path = path
                .canonicalize()
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            assert!(path.is_file(), "{name} must be a regular file");
            path
        };
        Some(Self {
            java: canonical("Java", java.into()),
            jar: canonical(
                "game jar",
                std::env::var_os("CHONKSTEP_CHONKCRAFT_JAR")
                    .expect("an explicitly requested game check requires its jar")
                    .into(),
            ),
            pack: canonical(
                "asset pack",
                std::env::var_os("CHONKSTEP_CHONKCRAFT_PACK")
                    .expect("an explicitly requested game check requires its licensed asset pack")
                    .into(),
            ),
        })
    }
}

fn events(session: &Session) -> Vec<(u64, Value)> {
    session
        .client_log("bwrap")
        .lines()
        .filter_map(|line| {
            let line = line.strip_prefix("CHONK_OBSERVER ")?;
            let (sequence, json) = line.split_once(' ')?;
            Some((
                sequence.parse().expect("observer sequence"),
                serde_json::from_str(json).expect("complete observer JSON line"),
            ))
        })
        .collect()
}

fn snapshot(session: &Session) -> Option<Value> {
    events(session)
        .into_iter()
        .rev()
        .find_map(|(_, event)| (event["event"] == "snapshot").then_some(event))
}

fn has_caption(value: &Value, caption: &str) -> bool {
    value["entries"]
        .as_array()
        .is_some_and(|entries| entries.iter().any(|entry| entry["caption"] == caption))
}

fn number(value: &Value, name: &str) -> f64 {
    value[name]
        .as_f64()
        .unwrap_or_else(|| panic!("missing {name}: {value}"))
}

fn mapped_game(session: &mut Session) -> WindowInfo {
    session
        .world()
        .unwrap()
        .windows
        .into_iter()
        .find(|window| window.mapped)
        .expect("this isolated session's only client remains mapped")
}

fn wait_menu(session: &mut Session, caption: &str) -> Value {
    poll_until(EVENT, caption, || {
        if let Some(status) = session.client_status("bwrap").unwrap() {
            panic!("game exited {status}; {}", session.client_log("bwrap"));
        }
        snapshot(session).filter(|value| has_caption(value, caption))
    })
    .unwrap_or_else(|error| {
        let _ = session.screenshot("game-menu-timeout");
        panic!(
            "{error}; artifacts: {}; observer: {}",
            session.dir.display(),
            session.client_log("bwrap")
        )
    })
}

fn launch(session: &mut Session, files: &GameFiles) {
    let classes = session.dir.join("observer");
    let private_home = session.dir.join("game-home");
    std::fs::create_dir(&classes).unwrap();
    std::fs::create_dir(&private_home).unwrap();
    let javac = files.java.with_file_name("javac");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/ChonkGameObserver.java");
    session
        .launch_isolated(
            &javac.to_string_lossy(),
            &[
                "-Xlint:all",
                "-Werror",
                "-d",
                &classes.to_string_lossy(),
                &fixture.to_string_lossy(),
            ],
        )
        .expect("run the supplied JDK compiler with private XDG directories");
    // Session owns and reaps the compiler even if this bounded wait fails.
    let compiled = poll_until(EVENT, "the observer compiler to finish", || {
        session.client_status("javac").unwrap()
    })
    .unwrap();
    assert!(
        compiled.success(),
        "observer compilation: {}",
        session.client_log("javac")
    );
    let display = session.x11_display().unwrap();
    let socket = format!("/tmp/.X11-unix/X{}", display.strip_prefix(':').unwrap());
    assert!(
        Path::new(&socket).exists(),
        "the exact nested X socket exists before sandboxing"
    );
    // Read-only host files allow the supplied JDK/jar/pack to load. /run and
    // /tmp are private; only this X socket is exposed. No network, live bus,
    // audio, DRM, input-device or user-profile writes are available.
    let mut args: Vec<String> = [
        "--die-with-parent",
        "--new-session",
        "--unshare-pid",
        "--unshare-net",
        "--ro-bind",
        "/",
        "/",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--tmpfs",
        "/run",
        "--dir",
        "/tmp/.X11-unix",
        "--dir",
        "/tmp/chonk-runtime",
        "--bind",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    args.extend([
        private_home.to_string_lossy().into_owned(),
        "/tmp/chonk-home".into(),
        "--ro-bind".into(),
        classes.to_string_lossy().into_owned(),
        "/tmp/chonk-observer".into(),
        "--ro-bind".into(),
        files.jar.to_string_lossy().into_owned(),
        "/tmp/chonk-game.jar".into(),
        "--ro-bind".into(),
        files.pack.to_string_lossy().into_owned(),
        "/tmp/chonk-assets.pack".into(),
        "--ro-bind".into(),
        files
            .java
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        "/tmp/chonk-jdk".into(),
        "--ro-bind".into(),
        socket.clone(),
        socket,
        "--clearenv".into(),
    ]);
    for (name, value) in [
        ("PATH", "/usr/bin:/bin"),
        ("LANG", "C.UTF-8"),
        ("DISPLAY", &display),
        ("XDG_RUNTIME_DIR", "/tmp/chonk-runtime"),
        ("XDG_CONFIG_HOME", "/tmp/chonk-home/config"),
        ("XDG_CACHE_HOME", "/tmp/chonk-home/cache"),
        ("XDG_DATA_HOME", "/tmp/chonk-home/data"),
        ("XDG_STATE_HOME", "/tmp/chonk-home/state"),
        ("CHONKCRAFT_HOME", "/tmp/chonk-home/game"),
        ("GDK_BACKEND", "x11"),
        ("LIBGL_ALWAYS_SOFTWARE", "1"),
        ("GALLIUM_DRIVER", "llvmpipe"),
    ] {
        args.extend(["--setenv".into(), name.into(), value.into()]);
    }
    args.extend([
        "/tmp/chonk-jdk/bin/java".into(),
        "-Xms64m".into(),
        "-Xmx512m".into(),
        "-Duser.home=/tmp/chonk-home".into(),
        "-Dchonkcraft.home=/tmp/chonk-home/game".into(),
        "-Duser.name=ChonkStep-Test".into(),
        "-Dawt.toolkit.name=XToolkit".into(),
        "-Dseven.controller.enabled=false".into(),
        "-Dchonkcraft.skipTitles=true".into(),
        "-Dchonkcraft.window=800x600".into(),
        "-Dchonkcraft.pack=/tmp/chonk-assets.pack".into(),
        "-cp".into(),
        "/tmp/chonk-observer:/tmp/chonk-game.jar".into(),
        "ChonkGameObserver".into(),
    ]);
    let borrowed: Vec<_> = args.iter().map(String::as_str).collect();
    session.launch_x11_isolated("bwrap", &borrowed).unwrap();
}

fn click_menu(session: &mut Session, caption: &str, next: &str, phase: &str) {
    let before = wait_menu(session, caption);
    let entry = before["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["caption"] == caption)
        .unwrap();
    let expected = (number(entry, "x"), number(entry, "y"));
    let window = mapped_game(session);
    // AWT's own content rectangle is bottom/right aligned inside the X window.
    // Its graphics transform accounts for toolkit HiDPI independently of the
    // compositor's output scale; do not apply either scale a second time.
    let sx = number(&before, "scale_x");
    let sy = number(&before, "scale_y");
    let origin = (
        f64::from(window.x) + f64::from(window.w) - number(&before, "w") * sx,
        f64::from(window.y) + f64::from(window.h) - number(&before, "h") * sy,
    );
    let target = (origin.0 + expected.0 * sx, origin.1 + expected.1 * sy);
    let serial = events(session)
        .last()
        .map(|(serial, _)| *serial)
        .unwrap_or(0);
    session.door().click(target.0, target.1).unwrap();
    for id in [JAVA_MOUSE_PRESSED, JAVA_MOUSE_RELEASED] {
        let received = poll_until(
            EVENT,
            "the real game to receive the exact menu click",
            || {
                events(session).into_iter().find_map(|(number, value)| {
                    (number > serial && value["event"] == "mouse" && value["id"] == id)
                        .then_some(value)
                })
            },
        )
        .unwrap_or_else(|error| panic!("{error}; {before}; {}", session.client_log("bwrap")));
        assert!((number(&received, "x") - expected.0).abs() <= 1.0
            && (number(&received, "y") - expected.1).abs() <= 1.0,
            "{phase}: AWT got {received}, expected {expected:?}; target={target:?}; window={window:?}; {before}");
    }
    let after = wait_menu(session, next);
    assert!(
        !has_caption(&after, caption),
        "the click must actually change the game's menu page"
    );
    session.screenshot(&format!("game-{phase}")).unwrap();
    eprintln!("Chonkcraft {phase}: {caption:?} -> {next:?}; physical={target:?}; awt={expected:?}; transform=({sx},{sy})");
}

fn run(scale: f32) {
    let Some(files) = GameFiles::requested() else {
        eprintln!("SKIP: real Chonkcraft requires explicit CHONKSTEP_CHONKCRAFT_JAVA/JAR/PACK; no game coverage claimed");
        return;
    };
    let mut session = Session::boot(&format!("chonkcraft-menu-{scale}"), SessionOptions {
        scale: Some(scale),
        config_extra: "omarchy_menu = false\nhyprland_config = false\nrestore_session = false\nshow_dock = false\n\
            [keybindings]\n\"super+f11\" = \"toggle-fullscreen\"\n".into(),
        ..Default::default()
    }).unwrap();
    launch(&mut session, &files);
    wait_menu(&mut session, "Campaign Game");
    session.screenshot("game-initial-menu").unwrap();
    click_menu(
        &mut session,
        "Campaign Game",
        "Previous Menu",
        "windowed-campaign",
    );
    click_menu(
        &mut session,
        "Previous Menu",
        "Campaign Game",
        "windowed-return",
    );
    let previous = mapped_game(&mut session);
    let old_snapshot = snapshot(&session).unwrap();
    session.door().chord(keys::LEFTMETA, KEY_F11).unwrap();
    poll_until(EVENT, "the game to enter compositor fullscreen", || {
        let world = session.world().ok()?;
        // Fullscreen keeps a transparent mapped frame record; content and
        // frame have the same output-sized rectangle with zero chrome.
        let undecorated = world
            .frames
            .iter()
            .filter(|frame| frame.mapped)
            .all(|frame| {
                (frame.x, frame.y, frame.w, frame.h) == (0, 0, world.output_w, world.output_h)
            });
        world.windows.into_iter().find(|window| {
            window.mapped
                && undecorated
                && window.x == 0
                && window.y == 0
                && window.w == world.output_w
                && window.h == world.output_h
        })
    })
    .unwrap_or_else(|error| {
        panic!(
            "{error}; world={:?}; artifacts={}",
            session.world(),
            session.dir.display()
        )
    });
    poll_until(EVENT, "AWT to consume the fullscreen geometry", || {
        let value = snapshot(&session)?;
        (value != old_snapshot && has_caption(&value, "Campaign Game")).then_some(())
    })
    .unwrap();
    click_menu(
        &mut session,
        "Campaign Game",
        "Previous Menu",
        "fullscreen-campaign",
    );
    click_menu(
        &mut session,
        "Previous Menu",
        "Campaign Game",
        "fullscreen-return",
    );
    session.door().chord(keys::LEFTMETA, KEY_F11).unwrap();
    poll_until(
        EVENT,
        "the game to return to its exact windowed rectangle",
        || {
            let current = mapped_game(&mut session);
            ((current.x, current.y, current.w, current.h)
                == (previous.x, previous.y, previous.w, previous.h))
                .then_some(())
        },
    )
    .unwrap();
}

#[test]
#[ignore = "requires private nested session and explicitly supplied game/JDK/licensed pack"]
fn real_game_menu_clicks_windowed_and_fullscreen_scale_one() {
    run(1.0);
}

#[test]
#[ignore = "requires private nested session and explicitly supplied game/JDK/licensed pack"]
fn real_game_menu_clicks_windowed_and_fullscreen_scale_fractional() {
    run(1.5);
}

#[test]
#[ignore = "requires private nested session and explicitly supplied game/JDK/licensed pack"]
fn real_game_menu_clicks_windowed_and_fullscreen_scale_two() {
    run(2.0);
}
