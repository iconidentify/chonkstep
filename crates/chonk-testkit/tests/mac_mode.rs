//! Inject physical seat events. DevTools only observes the private page and
//! selects the starting editing context, never dispatches keyboard/clipboard API.
#[path = "support/browser.rs"]
mod browser;
use browser::Browser;
use chonk_testkit::{poll_until, Session, SessionOptions};
use serde_json::Value;
use std::{path::PathBuf, time::Duration};
const WAIT: Duration = Duration::from_secs(12);
const CMD: u32 = 125;
const CTRL: u32 = 29;
const SHIFT: u32 = 42;
const CONTENT: &str = "Mac clipboard: café — 日本語 🍎\nsecond line";

fn chord(s: &mut Session, modifiers: &[u32], code: u32) {
    for &m in modifiers {
        s.door().key(m, true).unwrap();
    }
    s.door().barrier().unwrap();
    s.door().tap_key(code).unwrap();
    for &m in modifiers.iter().rev() {
        s.door().key(m, false).unwrap();
    }
    s.door().barrier().unwrap();
}
fn browser(s: &mut Session, platform: &str, suffix: &str) -> Browser {
    assert!(chonk_testkit::require_client("chromium"));
    let profile = s.dir.join(format!("browser-{suffix}"));
    std::fs::create_dir(&profile).unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/mac-mode.html");
    let url = format!("file://{}?{suffix}", fixture.display());
    let args = vec![
        format!("--ozone-platform={platform}"),
        format!("--user-data-dir={}", profile.display()),
        format!("--app={url}"),
        "--window-size=520,400".into(),
        "--remote-debugging-address=127.0.0.1".into(),
        "--remote-debugging-port=0".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--disable-background-networking".into(),
        "--disable-component-update".into(),
        "--disable-default-apps".into(),
        "--disable-extensions".into(),
        "--disable-sync".into(),
        "--password-store=basic".into(),
    ];
    let args: Vec<_> = args.iter().map(String::as_str).collect();
    if platform == "x11" {
        s.launch_x11_isolated("chromium", &args).unwrap();
    } else {
        s.launch_isolated("chromium", &args).unwrap();
    }
    let mut b = poll_until(Duration::from_secs(60), "private browser page", || {
        Browser::connect(&profile, &url).ok()
    })
    .unwrap();
    poll_until(WAIT, "loaded Mac fixture", || {
        (b.evaluate("!!document.querySelector('#source')").ok()? == true).then_some(())
    })
    .unwrap();
    b.evaluate(&format!("document.title='Mac Probe {suffix}'; true"))
        .unwrap();
    let window = s.wait_for_window(&format!("Mac Probe {suffix}")).unwrap();
    let frame = s.world().unwrap().frame_of(window.id).cloned();
    if let Some(f) = frame {
        let x = if platform == "x11" { 650.0 } else { 40.0 };
        s.door()
            .drag_to((f.x as f64 + 180.0, f.y as f64 + 10.0), (x + 180.0, 50.0))
            .unwrap();
        s.door().button("left", false).unwrap();
        s.door().barrier().unwrap();
    }
    b
}
fn focus(s: &mut Session, b: &mut Browser, id: &str) {
    let title = b.evaluate("document.title").unwrap().as_str().unwrap().to_owned();
    let window = s.world().unwrap().window_matching(&title).unwrap().clone();
    let rect=b.evaluate(&format!("(()=>{{const r=document.getElementById('{id}').getBoundingClientRect();return {{x:r.x+10,y:r.y+10,w:innerWidth,h:innerHeight,dpr:devicePixelRatio}}}})()")).unwrap();
    let n = |key: &str| rect[key].as_f64().unwrap();
    let x = window.x as f64 + (window.w as f64 - n("w") * n("dpr")).max(0.0) + n("x") * n("dpr");
    let y = window.y as f64 + (window.h as f64 - n("h") * n("dpr")).max(0.0) + n("y") * n("dpr");
    s.door().click(x, y).unwrap();
    poll_until(WAIT, "focused browser field", || {
        (b.evaluate(&format!("document.hasFocus() && document.activeElement.id === '{id}'"))
            .ok()?
            == true)
            .then_some(())
    })
    .unwrap_or_else(|e| {
        let _ = s.screenshot("focus-failure");
        panic!(
            "{e}: point {x},{y}, window {window:?}, rect {rect}, DOM {}, world {:?}",
            b.evaluate("({focus:document.hasFocus(),active:document.activeElement.id})")
                .unwrap(),
            s.world().unwrap()
        );
    });
    s.door().barrier().unwrap();
}
fn expect(b: &mut Browser, id: &str, expected: &str) {
    let mut last = Value::Null;
    poll_until(WAIT, "actual edited text", || {
        last = b.evaluate(&format!("document.getElementById('{id}').value")).unwrap();
        (last == expected).then_some(())
    })
    .unwrap_or_else(|e| {
        panic!(
            "{e}: expected {expected:?}, got {last}; events {}",
            b.evaluate("events").unwrap()
        )
    });
}
fn boot(name: &str) -> Session {
    Session::boot(
        name,
        SessionOptions {
            config_extra: "interaction_mode = 'mac'\nshow_dock = false\nhyprland_config = false\n".into(),
            ..Default::default()
        },
    )
    .unwrap()
}
#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_mode"]
fn native_and_xwayland_copy_cut_paste_undo_and_releases() {
    let mut s = boot("mac-clipboard");
    let mut native = browser(&mut s, "wayland", "native");
    focus(&mut s, &mut native, "source");
    chord(&mut s, &[CMD], 30); // A
    chord(&mut s, &[CMD], 46); // C
    focus(&mut s, &mut native, "target");
    chord(&mut s, &[CMD], 47); // V
    expect(&mut native, "target", CONTENT);
    chord(&mut s, &[CMD], 30);
    chord(&mut s, &[CMD], 45); // X
    expect(&mut native, "target", "");
    chord(&mut s, &[CMD], 44); // Z
    expect(&mut native, "target", CONTENT);
    chord(&mut s, &[CMD, SHIFT], 44);
    expect(&mut native, "target", "");
    let mut x11 = browser(&mut s, "x11", "x11");
    focus(&mut s, &mut x11, "target");
    chord(&mut s, &[CMD], 47);
    expect(&mut x11, "target", CONTENT);
    chord(&mut s, &[CMD], 30);
    chord(&mut s, &[CMD], 46);
    focus(&mut s, &mut native, "target");
    chord(&mut s, &[CMD], 47);
    expect(&mut native, "target", CONTENT);
    // Command released before the character: no synthetic Control remains.
    s.door().key(CMD, true).unwrap();
    s.door().key(30, true).unwrap();
    s.door().key(CMD, false).unwrap();
    s.door().key(30, false).unwrap();
    s.door().tap_key(48).unwrap(); // b replaces selection
    s.door().barrier().unwrap();
    expect(&mut native, "target", "b");
    // Invalid reload cannot silently replace Mac with the legacy profile.
    std::fs::write(
        s.dir.join("config/chonkstep/config.toml"),
        "interaction_mode = 'invalid'",
    )
    .unwrap();
    s.request_reload().unwrap();
    poll_until(WAIT, "rejected reload", || {
        s.log().contains("retaining working configuration").then_some(())
    })
    .unwrap();
    chord(&mut s, &[CMD], 30);
    chord(&mut s, &[CMD], 47);
    expect(&mut native, "target", CONTENT);
    // Physical Control still behaves normally, too.
    chord(&mut s, &[CTRL], 30);
    s.door().tap_key(46).unwrap();
    s.door().barrier().unwrap();
    expect(&mut native, "target", "c");
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_mode"]
fn terminal_clipboard_control_and_copy_quit_paste() {
    let mut s = boot("mac-terminal");
    let mut b = browser(&mut s, "wayland", "native");
    focus(&mut s, &mut b, "source");
    chord(&mut s, &[CMD], 30);
    chord(&mut s, &[CMD], 46);
    // Quit the actual source through the desktop shortcut, then paste into a
    // terminal. No test process becomes clipboard owner on its behalf.
    chord(&mut s, &[CMD], 16);
    s.wait_for_window_gone("Mac Probe native").unwrap();
    let output = s.dir.join("pty-bytes");
    let script = s.dir.join("terminal.py");
    std::fs::write(&script, "import os,sys,tty\ntty.setraw(0)\nf=open(sys.argv[1],'wb',buffering=0)\nos.write(1,b'TERMINAL_COPY\\r\\n')\nwhile True:\n b=os.read(0,4096)\n if not b: break\n f.write(b)\n").unwrap();
    assert!(chonk_testkit::require_client("foot"));
    s.launch_isolated(
        "foot",
        &[
            "--title=Mac Terminal",
            "--app-id=foot",
            "--window-size-chars=60x12",
            "python3",
            script.to_str().unwrap(),
            output.to_str().unwrap(),
        ],
    )
    .unwrap();
    let terminal = s.wait_for_window("Mac Terminal").unwrap();
    poll_until(WAIT, "terminal raw input ready", || output.exists().then_some(())).unwrap();
    s.door()
        .click(terminal.x as f64 + 30.0, terminal.y as f64 + 100.0)
        .unwrap();
    chord(&mut s, &[CMD], 47);
    poll_until(WAIT, "paste after source quit", || {
        (std::fs::read(&output).ok()? == CONTENT.replace('\n', "\r").as_bytes()).then_some(())
    })
    .unwrap();
    chord(&mut s, &[CTRL], 46);
    poll_until(WAIT, "physical Control-C reaches PTY", || {
        std::fs::read(&output).ok()?.ends_with(&[3]).then_some(())
    })
    .unwrap();
    let before = std::fs::read(&output).unwrap();
    // Triple-click selects the printed line. PRIMARY selection alone is not
    // the clipboard; Command-C must publish the terminal selection.
    s.door()
        .click(terminal.x as f64 + 30.0, terminal.y as f64 + 10.0)
        .unwrap();
    s.door()
        .click(terminal.x as f64 + 30.0, terminal.y as f64 + 10.0)
        .unwrap();
    s.door()
        .click(terminal.x as f64 + 30.0, terminal.y as f64 + 10.0)
        .unwrap();
    chord(&mut s, &[CMD], 46);
    assert_eq!(
        std::fs::read(&output).unwrap(),
        before,
        "Command-C must not write an interrupt or any PTY input"
    );
    let mut destination = browser(&mut s, "wayland", "destination");
    focus(&mut s, &mut destination, "target");
    chord(&mut s, &[CMD], 47);
    expect(&mut destination, "target", "TERMINAL_COPY\n");
    chord(&mut s, &[CMD], 15);
    chord(&mut s, &[CMD], 17); // close the single-window terminal
    s.wait_for_window_gone("Mac Terminal").unwrap();
    assert!(s.world().unwrap().window_matching("Mac Probe destination").is_some());
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_mode"]
fn application_hide_desktop_switching_and_fullscreen_spaces() {
    let mut s = boot("mac-desktop");
    s.launch_isolated("foot", &["--title=Mac First", "--app-id=first", "sleep", "120"])
        .unwrap();
    let first = s.wait_for_window("Mac First").unwrap();
    s.launch_isolated("foot", &["--title=Mac Second", "--app-id=second", "sleep", "120"])
        .unwrap();
    let second = s.wait_for_window("Mac Second").unwrap();
    chord(&mut s, &[CMD], 15);
    poll_until(WAIT, "Command-Tab switches applications", || {
        (s.world().ok()?.logical_focus == Some(first.id)).then_some(())
    })
    .unwrap();
    chord(&mut s, &[CMD], 35); // H
    poll_until(WAIT, "hide reveals other app", || {
        let w = s.world().ok()?;
        (!w.frame_of(first.id)?.mapped && w.logical_focus == Some(second.id)).then_some(())
    })
    .unwrap();
    chord(&mut s, &[CMD], 15);
    poll_until(WAIT, "switch unhides app", || {
        let w = s.world().ok()?;
        (w.frame_of(first.id)?.mapped && w.logical_focus == Some(first.id)).then_some(())
    })
    .unwrap();
    chord(&mut s, &[], 87); // F11 show desktop
    assert!(!s.world().unwrap().frame_of(first.id).unwrap().mapped);
    chord(&mut s, &[], 87);
    assert!(s.world().unwrap().frame_of(first.id).unwrap().mapped);
    chord(&mut s, &[CMD, CTRL], 33); // F
    poll_until(WAIT, "dedicated fullscreen Space", || {
        let w = s.world().ok()?;
        (w.workspace_count == 2 && w.current_workspace == 1).then_some(())
    })
    .unwrap();
    chord(&mut s, &[CTRL], 105);
    assert_eq!(s.world().unwrap().current_workspace, 0);
    chord(&mut s, &[CTRL], 106);
    assert_eq!(s.world().unwrap().current_workspace, 1);
    chord(&mut s, &[CTRL], 106);
    assert_eq!(
        s.world().unwrap().workspace_count,
        2,
        "edge navigation cannot create desktops"
    );
    chord(&mut s, &[CMD, CTRL], 33);
    poll_until(WAIT, "fullscreen Space removed on exit", || {
        let w = s.world().ok()?;
        (w.workspace_count == 1 && w.current_workspace == 0).then_some(())
    })
    .unwrap();
    chord(&mut s, &[CTRL], 103); // Mission Control
    poll_until(WAIT, "Mission Control", || s.world().ok()?.overview.map(|_| ())).unwrap();
    chord(&mut s, &[], 1); // Escape
    assert!(s.world().unwrap().overview.is_none());
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_mode"]
fn force_quit_requires_application_selection_and_confirmation() {
    let mut s = boot("mac-force-quit");
    s.launch_isolated("foot", &["--title=Force Quit Target", "--app-id=foot", "sleep", "120"])
        .unwrap();
    let target = s.wait_for_window("Force Quit Target").unwrap();
    chord(&mut s, &[CMD, 56], 1);
    poll_until(WAIT, "Force Quit chooser", || {
        (!s.world().ok()?.menus().is_empty()).then_some(())
    })
    .unwrap();
    chord(&mut s, &[], 1);
    assert!(
        s.world().unwrap().windows.iter().any(|w| w.id == target.id),
        "Escape must not close an application"
    );
    chord(&mut s, &[CMD, 56], 1);
    chord(&mut s, &[], 108); // select application after Cancel
    chord(&mut s, &[], 106); // open confirmation
    poll_until(WAIT, "Force Quit confirmation", || {
        (s.world().ok()?.menus().len() == 2).then_some(())
    })
    .unwrap();
    chord(&mut s, &[], 28); // default Cancel
    assert!(
        s.world().unwrap().windows.iter().any(|w| w.id == target.id),
        "confirmation defaults to Cancel"
    );
    chord(&mut s, &[CMD, 56], 1);
    chord(&mut s, &[], 108);
    chord(&mut s, &[], 106);
    chord(&mut s, &[], 108);
    chord(&mut s, &[], 28); // explicit Force Quit
    s.wait_for_window_gone("Force Quit Target").unwrap();
    // The same chooser must really disconnect X11 clients too, including those
    // advertising WM_DELETE_WINDOW; polite close is not a force-quit fallback.
    let _b = browser(&mut s, "x11", "force-quit");
    chord(&mut s, &[CMD, 56], 1);
    chord(&mut s, &[], 108);
    chord(&mut s, &[], 106);
    chord(&mut s, &[], 108);
    chord(&mut s, &[], 28);
    s.wait_for_window_gone("Mac Probe force-quit").unwrap();
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_mode"]
fn screenshot_file_and_clipboard_destinations_are_independent() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command, Stdio};
    let root = chonk_testkit::session_dir("mac-capture");
    let exports = root.join("exports");
    let fixture_bin = root.join("config/chonkstep/bin");
    let mut paths = vec![fixture_bin.clone()];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let mut s = Session::boot(
        "mac-capture",
        SessionOptions {
            config_extra: "interaction_mode = 'mac'\nhyprland_config = false\nshow_dock = false\n".into(),
            config_files: vec![
                ("bin/xdg-open".into(), "#!/bin/sh\n: > \"$MAC_REVIEW_LOG\"\n".into()),
                ("bin/notify-send".into(), "#!/bin/sh\nexit 0\n".into()),
                ("bin/omacut".into(), "#!/bin/sh\n: > \"$MAC_REVIEW_LOG\"\n".into()),
            ],
            env: vec![
                ("OMARCHY_SCREENSHOT_DIR".into(), exports.display().to_string()),
                ("OMARCHY_SCREENRECORD_DIR".into(), exports.display().to_string()),
                (
                    "PATH".into(),
                    std::env::join_paths(paths).unwrap().into_string().unwrap(),
                ),
                (
                    "MAC_REVIEW_LOG".into(),
                    root.join("review-launched").display().to_string(),
                ),
            ],
            ..Default::default()
        },
    )
    .unwrap();
    for name in ["xdg-open", "notify-send", "omacut"] {
        std::fs::set_permissions(fixture_bin.join(name), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut b = browser(&mut s, "wayland", "capture");
    focus(&mut s, &mut b, "source");
    chord(&mut s, &[CMD], 30);
    chord(&mut s, &[CMD], 46);
    let files = || {
        std::fs::read_dir(&exports)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension().is_some_and(|s| s == "png") && !p.file_name().unwrap().to_string_lossy().starts_with('.')
            })
            .collect::<Vec<_>>()
    };
    chord(&mut s, &[CMD, SHIFT], 4);
    let path = poll_until(WAIT, "Command-Shift-3 saves PNG", || files().into_iter().next()).unwrap();
    let screenshot = chonk_testkit::Screenshot::load(&path).unwrap();
    assert_eq!((screenshot.width, screenshot.height), (1280, 800));
    focus(&mut s, &mut b, "target");
    chord(&mut s, &[CMD], 47);
    expect(&mut b, "target", CONTENT);
    chord(&mut s, &[CMD, CTRL, SHIFT], 4);
    let png = root.join("clipboard.png");
    // Retry only until the image MIME appears; never replace the clipboard.
    poll_until(WAIT, "clipboard PNG", || {
        let mut child = Command::new("wl-paste")
            .args(["--type", "image/png", "--no-newline"])
            .env("WAYLAND_DISPLAY", &s.wayland_display)
            .stdout(Stdio::from(std::fs::File::create(&png).unwrap()))
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let result = poll_until(Duration::from_secs(2), "clipboard reader exits", || {
            child.try_wait().ok().flatten()
        });
        if result.is_err() {
            let _ = child.kill();
            let _ = poll_until(Duration::from_secs(2), "clipboard reader reaped", || {
                child.try_wait().ok().flatten()
            });
            return None;
        }
        result.unwrap().success().then_some(())
    })
    .unwrap();
    chonk_testkit::Screenshot::load(&png).unwrap();
    assert_eq!(files().len(), 1, "clipboard capture must not save a user file");
    assert!(
        !root.join("review-launched").exists(),
        "Mac capture must not open a viewer"
    );
    chord(&mut s, &[CMD, SHIFT], 5);
    chord(&mut s, &[], 1);
    assert_eq!(files().len(), 1, "Escape cancels the selection");
    assert!(chonk_testkit::require_client("wf-recorder"));
    assert!(chonk_testkit::require_client("ffprobe"));
    chord(&mut s, &[CMD, SHIFT], 6); // Command-Shift-5 toolbar
    chord(&mut s, &[], 6); // record area
    s.door().drag_to((700.0, 300.0), (1000.0, 500.0)).unwrap();
    s.door().button("left", false).unwrap();
    chord(&mut s, &[], 28);
    let end = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < end {
        s.door().motion(1100.0, 700.0).unwrap();
        s.door().barrier().unwrap();
        std::thread::sleep(Duration::from_millis(40));
    }
    chord(&mut s, &[CMD, CTRL], 1);
    let recording = poll_until(WAIT, "Mac recording saved", || {
        std::fs::read_dir(&exports).ok()?.flatten().map(|e| e.path()).find(|p| {
            p.extension().is_some_and(|s| s == "mp4") && !p.file_name().unwrap().to_string_lossy().starts_with('.')
        })
    })
    .unwrap();
    let probe = root.join("recording-probe.json");
    let mut child = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=width,height:format=duration",
            "-of",
            "json",
        ])
        .arg(recording)
        .stdout(Stdio::from(std::fs::File::create(&probe).unwrap()))
        .spawn()
        .unwrap();
    assert!(
        poll_until(WAIT, "recording decodes", || child.try_wait().ok().flatten())
            .unwrap()
            .success()
    );
    let info: Value = serde_json::from_slice(&std::fs::read(probe).unwrap()).unwrap();
    assert_eq!(info["streams"][0]["width"], 300);
    assert_eq!(info["streams"][0]["height"], 200);
    assert!(info["format"]["duration"].as_str().unwrap().parse::<f64>().unwrap() > 1.0);
    assert!(
        !root.join("review-launched").exists(),
        "Mac recording must not open an editor"
    );
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_mode"]
fn navigation_release_focus_resume_and_passthrough() {
    let mut s = boot("mac-input-lifecycle");
    let mut native = browser(&mut s, "wayland", "native");
    focus(&mut s, &mut native, "nav");
    chord(&mut s, &[CMD], 108); // document end
    chord(&mut s, &[56], 105); // previous word
    chord(&mut s, &[CMD, SHIFT], 106); // select to line end
    s.door().tap_key(45).unwrap();
    s.door().barrier().unwrap();
    expect(&mut native, "nav", "one two x");
    // Resume balances the remapped Home rather than sending a stray Left up.
    native.evaluate("events.length=0;true").unwrap();
    s.door().key(CMD, true).unwrap();
    s.door().key(105, true).unwrap();
    s.door().barrier().unwrap();
    s.door().reset_input(true).unwrap();
    s.door().barrier().unwrap();
    poll_until(WAIT, "balanced Home after resume", || {
        (native
            .evaluate("events.filter(e=>e.key==='Home' && e.type==='keyup').length")
            .ok()?
            == 1)
            .then_some(())
    })
    .unwrap();
    assert_eq!(
        native
            .evaluate("events.filter(e=>e.key==='ArrowLeft' && e.type==='keyup').length")
            .unwrap(),
        0
    );
    s.door().tap_key(48).unwrap();
    s.door().barrier().unwrap();
    expect(&mut native, "nav", "bone two x");
    // A held translated navigation press cannot cross focus through enter or release.
    let mut x11 = browser(&mut s, "x11", "x11");
    focus(&mut s, &mut native, "nav");
    s.door().key(CMD, true).unwrap();
    s.door().key(105, true).unwrap();
    s.door().barrier().unwrap();
    x11.evaluate("events.length=0;true").unwrap();
    focus(&mut s, &mut x11, "target");
    s.door().key(105, false).unwrap();
    s.door().key(CMD, false).unwrap();
    s.door().barrier().unwrap();
    assert_eq!(x11.evaluate("events.filter(e=>e.key==='Home').length").unwrap(), 0);
    // A remote/game passthrough profile owns both Command input and desktop shortcuts.
    let app = s.world().unwrap().window_matching("Mac Probe x11").unwrap().app.clone();
    s.rewrite_config(&format!("interaction_mode='mac'\nhyprland_config=false\nshow_dock=false\nomarchy_shell=false\n[mac.applications]\n{}='passthrough'\n",serde_json::to_string(&app).unwrap())).unwrap();
    s.request_reload().unwrap();
    poll_until(WAIT, "profile reload", || {
        s.log().contains("reload requested").then_some(())
    })
    .unwrap();
    s.door().barrier().unwrap();
    x11.evaluate("events.length=0;true").unwrap();
    chord(&mut s, &[CMD], 35);
    poll_until(WAIT, "raw Command-H", || {
        (x11.evaluate("events.some(e=>e.key==='h' && e.type==='keydown' && e.meta && !e.ctrl)")
            .ok()?
            == true)
            .then_some(())
    })
    .unwrap();
    assert!(s
        .world()
        .unwrap()
        .frame_of(s.world().unwrap().window_matching("Mac Probe x11").unwrap().id)
        .is_some());
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_mode"]
fn overlapping_navigation_both_commands_caps_lock_and_mode_rollback() {
    let mut s = boot("mac-key-balance");
    let mut b = browser(&mut s, "wayland", "balance");
    focus(&mut s, &mut b, "source");
    s.door().tap_key(58).unwrap(); // Caps Lock must not change shortcut matching.
    s.door().key(CMD, true).unwrap();
    s.door().key(126, true).unwrap();
    s.door().tap_key(30).unwrap();
    s.door().key(CMD, false).unwrap();
    s.door().tap_key(46).unwrap();
    s.door().key(126, false).unwrap();
    s.door().tap_key(58).unwrap();
    focus(&mut s, &mut b, "target");
    chord(&mut s, &[CMD], 47);
    expect(&mut b, "target", CONTENT);
    b.evaluate("events.length=0;true").unwrap();
    s.door().key(CMD, true).unwrap();
    s.door().key(105, true).unwrap(); // translated Home
    s.door().key(102, true).unwrap();
    s.door().key(102, false).unwrap(); // overlapping physical Home
    s.door().key(105, false).unwrap();
    s.door().key(CMD, false).unwrap();
    s.door().barrier().unwrap();
    poll_until(WAIT, "overlapping Home is balanced", || {
        (b.evaluate("events.filter(e=>e.key==='Home' && e.type==='keyup').length")
            .ok()?
            == 1)
            .then_some(())
    })
    .unwrap();
    assert_eq!(
        b.evaluate("events.filter(e=>e.key==='Home' && e.type==='keydown').length")
            .unwrap(),
        1
    );
    b.evaluate("events.length=0;true").unwrap();
    s.door().key(CMD, true).unwrap();
    s.door().key(105, true).unwrap();
    s.door().barrier().unwrap();
    let reloads = s.log().matches("reload requested").count();
    s.rewrite_config("interaction_mode='desktop'\nhyprland_config=false\nshow_dock=false\nomarchy_shell=false\n")
        .unwrap();
    s.request_reload().unwrap();
    poll_until(WAIT, "rollback applied", || {
        (s.log().matches("reload requested").count() > reloads).then_some(())
    })
    .unwrap();
    s.door().barrier().unwrap();
    s.door().key(105, false).unwrap();
    s.door().key(CMD, false).unwrap();
    s.door().barrier().unwrap();
    poll_until(WAIT, "translated release survives mode rollback", || {
        (b.evaluate("events.filter(e=>e.key==='Home' && e.type==='keyup').length")
            .ok()?
            == 1)
            .then_some(())
    })
    .unwrap();
    assert_eq!(
        b.evaluate("events.filter(e=>e.key==='ArrowLeft' && e.type==='keyup').length")
            .unwrap(),
        0
    );
    chord(&mut s, &[CTRL], 30);
    s.door().tap_key(30).unwrap();
    s.door().barrier().unwrap();
    expect(&mut b, "target", "a");
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_mode"]
fn german_and_french_layouts_use_their_own_letters_and_shifted_digits() {
    for (layout, z) in [("de", 21), ("fr", 17)] {
        let name = format!("mac-layout-{layout}");
        let exports = chonk_testkit::session_dir(&name).join("exports");
        let mut s = Session::boot(
            &name,
            SessionOptions {
                config_extra: "interaction_mode='mac'\nhyprland_config=false\nshow_dock=false\n".into(),
                env: vec![
                    ("XKB_DEFAULT_LAYOUT".into(), layout.into()),
                    ("OMARCHY_SCREENSHOT_DIR".into(), exports.display().to_string()),
                ],
                ..Default::default()
            },
        )
        .unwrap();
        let mut b = browser(&mut s, "wayland", layout);
        focus(&mut s, &mut b, "source");
        let a = if layout == "fr" { 16 } else { 30 };
        chord(&mut s, &[CMD], a);
        chord(&mut s, &[CMD], 46);
        focus(&mut s, &mut b, "target");
        chord(&mut s, &[CMD], 47);
        expect(&mut b, "target", CONTENT);
        chord(&mut s, &[CMD], z);
        expect(&mut b, "target", "");
        chord(&mut s, &[CMD, SHIFT], z);
        expect(&mut b, "target", CONTENT);
        // French has a double quote on this key's unshifted level.
        chord(&mut s, &[CMD, SHIFT], 4);
        poll_until(WAIT, "layout-aware screenshot", || {
            let path = std::fs::read_dir(&exports)
                .ok()?
                .flatten()
                .map(|e| e.path())
                .find(|p| {
                    p.extension().is_some_and(|s| s == "png")
                        && !p.file_name().unwrap().to_string_lossy().starts_with('.')
                })?;
            chonk_testkit::Screenshot::load(&path).ok()
        })
        .unwrap();
    }
}

fn type_ascii(s: &mut Session, text: &str) {
    let letters = "qwertyuiopasdfghjklzxcvbnm";
    let codes = [
        16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 30, 31, 32, 33, 34, 35, 36, 37, 38, 44, 45, 46, 47, 48, 49, 50,
    ];
    for c in text.chars() {
        let (code, shift) = if let Some(index) = letters.find(c) {
            (codes[index], false)
        } else if c.is_ascii_digit() {
            (if c == '0' { 11 } else { c as u32 - '0' as u32 + 1 }, false)
        } else {
            match c {
                '/' => (53, false),
                '-' => (12, false),
                '_' => (12, true),
                '.' => (52, false),
                ' ' => (57, false),
                _ => panic!("unsupported fixture character {c}"),
            }
        };
        if shift {
            s.door().key(SHIFT, true).unwrap();
        }
        s.door().tap_key(code).unwrap();
        if shift {
            s.door().key(SHIFT, false).unwrap();
        }
    }
    s.door().barrier().unwrap();
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_mode"]
fn nautilus_copies_files_using_command_shortcuts() {
    if !chonk_testkit::require_client("nautilus") {
        return;
    }
    let mut s = boot("mac-files");
    let source = s.dir.join("source");
    let destination = s.dir.join("destination");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&destination).unwrap();
    let bytes = "file clipboard: café 日本語 🍎\n";
    std::fs::write(source.join("clipboard.txt"), bytes).unwrap();
    s.launch_isolated(
        "env",
        &[
            "GSETTINGS_BACKEND=memory",
            "XDG_SESSION_TYPE=wayland",
            "GTK_A11Y=none",
            "nautilus",
            "--new-window",
            source.to_str().unwrap(),
        ],
    )
    .unwrap();
    let window = s.wait_for_window("source").unwrap();
    s.door()
        .click(
            window.x as f64 + window.w as f64 * 0.7,
            window.y as f64 + window.h as f64 * 0.65,
        )
        .unwrap();
    chord(&mut s, &[CMD], 30);
    chord(&mut s, &[CMD], 46);
    chord(&mut s, &[CMD], 38); // location
    type_ascii(&mut s, destination.to_str().unwrap());
    chord(&mut s, &[], 28);
    s.wait_for_window("destination").unwrap();
    chord(&mut s, &[CMD], 47);
    poll_until(WAIT, "Nautilus native file copy", || {
        (std::fs::read_to_string(destination.join("clipboard.txt")).ok()? == bytes).then_some(())
    })
    .unwrap_or_else(|e| {
        let _ = s.screenshot("files-failure");
        panic!("{e}: {:?}", s.world().unwrap());
    });
    assert_eq!(std::fs::read_to_string(source.join("clipboard.txt")).unwrap(), bytes);
    chord(&mut s, &[CMD], 16);
    s.wait_for_window_gone("destination").unwrap();
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_mode"]
fn libreoffice_and_browser_exchange_document_text() {
    if !chonk_testkit::require_client("libreoffice") {
        return;
    }
    let mut s = boot("mac-office");
    let document = s.dir.join("mac-office.fodt");
    let text = "Office → Browser: café 日本語 🍎";
    std::fs::write(&document,format!(r#"<?xml version="1.0" encoding="UTF-8"?><office:document xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" office:version="1.2" office:mimetype="application/vnd.oasis.opendocument.text"><office:body><office:text><text:p>{text}</text:p></office:text></office:body></office:document>"#)).unwrap();
    let profile = format!(
        "-env:UserInstallation=file://{}",
        s.dir.join("office-profile").display()
    );
    s.launch_isolated(
        "env",
        &[
            "GSETTINGS_BACKEND=memory",
            "XDG_SESSION_TYPE=wayland",
            "NO_AT_BRIDGE=1",
            "SAL_USE_VCLPLUGIN=gtk3",
            "libreoffice",
            &profile,
            "--writer",
            "--norestore",
            "--nologo",
            "--nofirststartwizard",
            document.to_str().unwrap(),
        ],
    )
    .unwrap();
    let window = poll_until(Duration::from_secs(60), "Writer document window", || {
        s.world()
            .ok()?
            .window_matching("mac-office.fodt")
            .filter(|w| w.mapped)
            .cloned()
    })
    .unwrap_or_else(|e| panic!("{e}: {}", s.client_log("env")));
    poll_until(Duration::from_secs(45), "Writer document painted", || {
        let shot = s.screenshot("writer-paint").ok()?;
        let mean = shot.mean_rgb(500, 300, 150, 150);
        (mean.iter().all(|channel| *channel > 180.0)).then_some(())
    })
    .unwrap();
    s.door()
        .click(window.x as f64 + window.w as f64 / 2.0, window.y as f64 + 250.0)
        .unwrap();
    s.screenshot("writer-ready").unwrap();
    chord(&mut s, &[CMD], 30);
    chord(&mut s, &[CMD], 46);
    let mut b = browser(&mut s, "wayland", "office");
    focus(&mut s, &mut b, "target");
    chord(&mut s, &[CMD], 47);
    poll_until(WAIT, "Writer text pasted into browser", || {
        let value = b.evaluate("document.getElementById('target').value").ok()?;
        (value.as_str()?.trim_end_matches('\n') == text).then_some(())
    })
    .unwrap_or_else(|error| {
        panic!(
            "{error}: pasted {}, events {}",
            b.evaluate("document.getElementById('target').value").unwrap(),
            b.evaluate("events").unwrap()
        )
    });
    focus(&mut s, &mut b, "source");
    chord(&mut s, &[CMD], 30);
    chord(&mut s, &[CMD], 46);
    chord(&mut s, &[CMD], 15); // return to Writer
    chord(&mut s, &[CMD], 30);
    chord(&mut s, &[CMD], 47);
    // Clipboard transfer is asynchronous. Wait for the new second line to be
    // painted before Save; a compositor barrier does not drain Writer's queue.
    poll_until(WAIT, "Writer pasted second line painted", || {
        let shot = s.screenshot("writer-pasted").ok()?;
        let ink = (272..288)
            .flat_map(|y| (280..450).map(move |x| (x, y)))
            .filter(|&(x, y)| shot.pixel(x, y)[..3].iter().all(|c| *c < 100))
            .count();
        (ink > 40).then_some(())
    })
    .unwrap();
    chord(&mut s, &[CMD], 31);
    poll_until(WAIT, "browser clipboard saved through Writer", || {
        let xml = std::fs::read_to_string(&document).ok()?;
        (xml.contains("second line") && xml.contains("日本語") && !xml.contains("Office → Browser")).then_some(())
    })
    .unwrap_or_else(|e| {
        let _ = s.screenshot("writer-save-failure");
        panic!("{e}: windows {:?}", s.world().unwrap().windows);
    });
}
