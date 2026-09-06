//! Real Chromium-family text selection and key repeat on Wayland and XWayland. Override
//! CHONKSTEP_TEST_BROWSER=microsoft-edge-stable to exercise Edge locally; CI
//! uses its installed Chromium. DevTools observes DOM state, never injects input.

#[path = "support/browser.rs"]
mod browser;

use std::path::PathBuf;
use std::time::Duration;

use chonk_testkit::{keys, poll_until, Session, SessionOptions, WindowInfo};
use serde_json::{json, Value};

use browser::Browser;

const EVENT: Duration = Duration::from_secs(10);
const SNAPSHOT: &str = "window.chonkProbe.snapshot()";
const TEXT: &str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const OMARCHY_INPUT: &str = "input {\n kb_model = apple\n kb_layout = us\n \
    kb_options = compose:caps,shift:both_capslock_cancel\n repeat_rate = 40\n repeat_delay = 250\n}\n\
    bind = SUPER, F12, workspace, 1\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Platform {
    Wayland,
    X11,
}

impl Platform {
    fn name(self) -> &'static str {
        match self {
            Self::Wayland => "wayland",
            Self::X11 => "x11",
        }
    }
}

fn window(session: &mut Session) -> WindowInfo {
    session
        .world()
        .unwrap()
        .window_matching("ChonkStep Selection Probe")
        .expect("the test browser window remains mapped")
        .clone()
}

fn value(browser: &mut Browser) -> Value {
    browser.evaluate(SNAPSHOT).expect("read private test page")
}

fn coordinate(value: &Value, key: &str) -> f64 {
    value[key]
        .as_f64()
        .unwrap_or_else(|| panic!("missing numeric {key}: {value}"))
}

fn viewport_origin(client: &WindowInfo, viewport: &Value) -> (f64, f64) {
    let dpr = coordinate(viewport, "dpr");
    // innerWidth/Height are integer CSS pixels. At fractional scale Chromium
    // may round a fullscreen viewport up beyond the surface by one device
    // pixel; that cannot move the content origin outside its client rectangle.
    (
        f64::from(client.x) + (f64::from(client.w) - coordinate(viewport, "width") * dpr).max(0.0),
        f64::from(client.y) + (f64::from(client.h) - coordinate(viewport, "height") * dpr).max(0.0),
    )
}

fn has_event(value: &Value, kind: &str) -> bool {
    value["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| event["type"] == kind)
}

fn wait_for_presented_extent(session: &mut Session, phase: &str) {
    poll_until(
        EVENT,
        &format!("the browser to commit its {phase} buffer"),
        || {
            let world = session.world().ok()?;
            let client = world.window_matching("ChonkStep Selection Probe")?;
            (client.presented_w >= client.w && client.presented_h >= client.h).then_some(())
        },
    )
    .unwrap_or_else(|error| {
        panic!(
            "{error}: compositor geometry can precede the client's matching commit: {:?}",
            session.world()
        )
    });
}

fn selection(session: &mut Session, browser: &mut Browser, phase: &str, id: &str) {
    let start = browser
        .evaluate(&format!("window.chonkProbe.point('{id}', 8)"))
        .unwrap();
    let end = browser
        .evaluate(&format!("window.chonkProbe.point('{id}', 24)"))
        .unwrap();
    for (point, expected) in [(&start, 8), (&end, 24)] {
        assert_eq!(
            point["offset"], expected,
            "DOM geometry must name the intended caret"
        );
        assert_eq!(point["sameNode"], true);
    }
    let viewport = value(browser);
    let dpr = coordinate(&viewport, "dpr");
    let client = window(session);
    // The page viewport is bottom/right aligned inside this app window. This
    // accounts for the browser's own titlebar without confusing it with the
    // compositor's server-side frame, which is outside `client` already.
    let origin = viewport_origin(&client, &viewport);
    let global = |point: &Value| {
        // X11 pointer events have integer device coordinates. Round into the
        // intended CSS pixel: truncating 73 * 1.5 to 109 lands in CSS pixel
        // 72 and falsely reports a one-pixel input transform error.
        (
            (origin.0 + coordinate(point, "x") * dpr).ceil(),
            (origin.1 + coordinate(point, "y") * dpr).ceil(),
        )
    };
    let from = global(&start);
    let to = global(&end);
    browser.evaluate("window.chonkProbe.reset()").unwrap();
    // Edge shows a native mini menu after selecting plain text. Like a user,
    // dismiss it with an outside click before beginning a new selection. DOM
    // range removal alone does not dismiss that browser-owned UI.
    session
        .door()
        .click(origin.0 + 40.0 * dpr, origin.1 + 40.0 * dpr)
        .unwrap();
    session.door().motion(from.0, from.1).unwrap();
    let hover = poll_until(EVENT, "browser hover before the selection grab", || {
        let observed = value(browser);
        let motion = observed["events"]
            .as_array()?
            .iter()
            .rev()
            .find(|event| event["type"] == "mousemove")?;
        ["x", "y"]
            .into_iter()
            .all(|axis| (coordinate(motion, axis) - coordinate(&start, axis)).abs() < 0.75)
            .then_some(observed)
    })
    .unwrap_or_else(|error| {
        let _ = session.screenshot(&format!("hover-timeout-{phase}-{id}"));
        panic!(
            "{error}: {phase} {id}, from={from:?}, to={to:?}, viewport={viewport}, observed={}",
            value(browser)
        );
    });
    let motion = hover["events"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|event| event["type"] == "mousemove")
        .unwrap();
    for axis in ["x", "y"] {
        assert!((coordinate(motion, axis) - coordinate(&start, axis)).abs() < 0.75,
            "viewport calibration failed before pressing: {phase}, {id}, {client:?}, {start}, {hover}");
    }
    // Do not confuse the popup-dismissal click with the drag's release.
    browser.evaluate("window.chonkProbe.reset()").unwrap();
    session.door().button("left", true).unwrap();
    for step in 1..=8 {
        let fraction = f64::from(step) / 8.0;
        session
            .door()
            .motion(
                from.0 + (to.0 - from.0) * fraction,
                from.1 + (to.1 - from.1) * fraction,
            )
            .unwrap();
    }
    session.door().button("left", false).unwrap();
    let observed = poll_until(
        EVENT,
        "the browser's completed physical selection drag",
        || {
            let observed = value(browser);
            has_event(&observed, "mouseup").then_some(observed)
        },
    )
    .unwrap();
    let report = json!({"phase":phase, "element":id, "viewport":viewport,
        "origin":origin, "from":from, "to":to, "start":start, "end":end, "observed":observed});
    std::fs::write(
        session.dir.join(format!("selection-{phase}-{id}.json")),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    let screenshot = session
        .screenshot(&format!("selection-{phase}-{id}"))
        .unwrap();
    let background = screenshot.mean_rgb(
        (origin.0 + 32.0 * dpr) as u32,
        // Keep clear of the browser's temporary fullscreen-exit banner.
        (origin.1 + (coordinate(&viewport, "height") - 24.0) * dpr) as u32,
        12,
        12,
    );
    for (actual, expected) in background.into_iter().zip([246.0, 244.0, 232.0]) {
        assert!(
            (actual - expected).abs() < 3.0,
            "DOM selection is insufficient if the browser did not render: {background:?}"
        );
    }
    let release = observed["events"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|event| event["type"] == "mouseup")
        .unwrap();
    for axis in ["x", "y"] {
        assert!(
            (coordinate(release, axis) - coordinate(&end, axis)).abs() < 0.75,
            "held-button {axis} is not the same coordinate space as hover: {report}"
        );
    }
    assert_eq!(observed["text"], &TEXT[8..24], "selection text: {report}");
    assert_eq!(observed["anchor"], 8, "selection anchor: {report}");
    assert_eq!(observed["focus"], 24, "selection endpoint: {report}");
    assert_eq!(observed["anchorId"], id);
    assert_eq!(observed["focusId"], id);
}

fn held_arrow(session: &mut Session, browser: &mut Browser, phase: &str) {
    browser
        .evaluate("window.chonkProbe.reset(); window.chonkProbe.caret(4)")
        .unwrap();
    session.door().key(keys::RIGHT, true).unwrap();
    let repeated = poll_until(
        EVENT,
        "the browser's held Right key to advance at least ten characters",
        || {
            let observed = value(browser);
            (observed["focus"].as_u64()? >= 14).then_some(observed)
        },
    )
    .unwrap();
    session.door().key(keys::RIGHT, false).unwrap();
    let released = poll_until(EVENT, "the browser's physical Right release", || {
        let observed = value(browser);
        has_event(&observed, "keyup").then_some(observed)
    })
    .unwrap();
    assert_eq!(released["focusId"], "editor");
    assert_eq!(released["text"], "");
    std::fs::write(
        session.dir.join(format!("keyboard-repeat-{phase}.json")),
        serde_json::to_vec_pretty(&json!({"repeated":repeated, "released":released})).unwrap(),
    )
    .unwrap();
}

fn field_repeat(session: &mut Session, browser: &mut Browser, phase: &str, id: &str) {
    let point = browser
        .evaluate(&format!("window.chonkProbe.field('{id}')"))
        .unwrap();
    let viewport = value(browser);
    let dpr = coordinate(&viewport, "dpr");
    let client = window(session);
    let origin = viewport_origin(&client, &viewport);
    session
        .door()
        .click(
            (origin.0 + coordinate(&point, "x") * dpr).ceil(),
            (origin.1 + coordinate(&point, "y") * dpr).ceil(),
        )
        .unwrap();
    poll_until(EVENT, "the physical click to focus the form field", || {
        (value(browser)["active"] == id).then_some(())
    })
    .unwrap();
    for (keycode, key, deleting) in [(30, "a", false), (14, "Backspace", true)] {
        // Removing DOM ranges here also removes Chromium's text-control
        // insertion point. Clear observation only; keep the clicked caret.
        browser.evaluate("window.chonkProbe.clearEvents()").unwrap();
        let initial = value(browser)["fields"][id]["value"]
            .as_str()
            .unwrap()
            .len();
        session.door().key(keycode, true).unwrap();
        let result = poll_until(
            EVENT,
            "the form field to receive repeated physical keys",
            || {
                let observed = value(browser);
                let length = observed["fields"][id]["value"].as_str()?.len();
                let repeats = observed["events"]
                    .as_array()?
                    .iter()
                    .filter(|event| {
                        event["type"] == "keydown" && event["key"] == key && event["repeat"] == true
                    })
                    .count();
                ((if deleting {
                    initial.saturating_sub(length) >= 6
                } else {
                    length >= 16
                }) && repeats >= 5)
                    .then_some(observed)
            },
        )
        .and_then(|repeated| {
            if !deleting {
                let reloads = session.log().matches("reload requested").count();
                session.request_reload()?;
                poll_until(EVENT, "the held key's Omarchy reload", || {
                    (session.log().matches("reload requested").count() > reloads).then_some(())
                })?;
                let before = value(browser)["fields"][id]["value"]
                    .as_str()
                    .unwrap()
                    .len();
                poll_until(EVENT, "typing to keep repeating after the reload", || {
                    (value(browser)["fields"][id]["value"].as_str()?.len() >= before + 6)
                        .then_some(())
                })?;
            }
            Ok(repeated)
        });
        // Balance the physical hold even if the browser fails the repeat assertion.
        session.door().key(keycode, false).unwrap();
        let repeated =
            result.unwrap_or_else(|error| panic!("{error}: {id} {key}: {}", value(browser)));
        let first = repeated["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|event| event["type"] == "keydown")
            .unwrap();
        assert_eq!(
            first["repeat"], false,
            "the initial press is not a repeat: {id} {key}"
        );
        let released = poll_until(EVENT, "the form field's key release", || {
            let observed = value(browser);
            has_event(&observed, "keyup").then_some(observed)
        })
        .unwrap();
        let settled = browser.evaluate(
            "new Promise(resolve => setTimeout(() => resolve(window.chonkProbe.snapshot()), 250))"
        ).unwrap();
        assert_eq!(
            settled["fields"][id], released["fields"][id],
            "repeat continued after release: {id} {key}"
        );
        assert_eq!(
            settled["events"], released["events"],
            "key events continued after release: {id} {key}"
        );
        std::fs::write(
            session.dir.join(format!("repeat-{phase}-{id}-{key}.json")),
            serde_json::to_vec_pretty(
                &json!({"repeated":repeated, "released":released, "settled":settled}),
            )
            .unwrap(),
        )
        .unwrap();
    }
}

fn modal_repeat(session: &mut Session, browser: &mut Browser) {
    let id = "textarea";
    let initial = value(browser)["fields"][id]["value"]
        .as_str()
        .unwrap()
        .len();
    session.door().key(30, true).unwrap();
    poll_until(EVENT, "text to repeat before opening Overview", || {
        (value(browser)["fields"][id]["value"].as_str()?.len() >= initial + 6).then_some(())
    })
    .unwrap();
    session.door().chord(keys::LEFTMETA, keys::UP).unwrap();
    poll_until(EVENT, "Overview to own focus away from the browser", || {
        let world = session.world().ok()?;
        (world.shells.iter().any(|shell| {
            shell.mapped && shell.above && shell.w == world.output_w && shell.h == world.output_h
        }) && browser.evaluate("document.hasFocus()").ok()? == false)
            .then_some(())
    })
    .unwrap();
    let before = value(browser);
    let settled = browser
        .evaluate(
            "new Promise(resolve => setTimeout(() => resolve(window.chonkProbe.snapshot()), 250))",
        )
        .unwrap();
    session.door().key(30, false).unwrap();
    assert_eq!(
        settled["fields"][id], before["fields"][id],
        "typing continued behind Overview"
    );
    session.door().tap_key(keys::ESC).unwrap();
    poll_until(
        EVENT,
        "the browser to regain focus on Overview cancellation",
        || {
            browser
                .evaluate("document.hasFocus()")
                .ok()?
                .as_bool()?
                .then_some(())
        },
    )
    .unwrap();
    field_repeat(session, browser, "after-overview", id);
}

fn run(name: &str, scale: f32, platform: Platform) {
    let program = std::env::var("CHONKSTEP_TEST_BROWSER").unwrap_or_else(|_| "chromium".into());
    if !chonk_testkit::require_client(&program) {
        return;
    }
    let mut session = Session::boot(
        name,
        SessionOptions {
            scale: Some(scale),
            env: if std::env::var_os("CHONKSTEP_BROWSER_TRACE").is_some() {
                vec![("WAYLAND_DEBUG".into(), "server".into())]
            } else {
                vec![]
            },
            config_extra: "show_dock = false\nomarchy_menu = false\nhyprland_config = true\n\
                [keybindings]\n\"super+up\" = \"overview\"\n"
                .into(),
            config_root_files: vec![("hypr/hyprland.conf".into(), OMARCHY_INPUT.into())],
            ..Default::default()
        },
    )
    .unwrap();
    let profile = session.dir.join("browser-profile");
    std::fs::create_dir(&profile).unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/browser-selection.html");
    let url = format!("file://{}?case={name}", fixture.display());
    let user_data = format!("--user-data-dir={}", profile.display());
    let app = format!("--app={url}");
    let ozone = format!("--ozone-platform={}", platform.name());
    let device_scale = format!("--force-device-scale-factor={scale}");
    let mut args = vec![
        &ozone,
        &user_data,
        &app,
        "--window-size=520,340",
        "--remote-debugging-address=127.0.0.1",
        "--remote-debugging-port=0",
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-background-networking",
        "--disable-component-update",
        "--disable-default-apps",
        "--disable-extensions",
        "--disable-sync",
        "--metrics-recording-only",
        "--password-store=basic",
    ];
    if platform == Platform::X11 {
        // X11 has no per-surface fractional-scale event. Pin the browser's
        // rendering scale while input still crosses the real XWayland server.
        args.push(&device_scale);
    }
    if std::env::var_os("CI").is_some() {
        // Same private-page software-renderer policy as the existing Chromium
        // regressions on hosted runners that disable unprivileged namespaces.
        args.extend(["--no-sandbox", "--use-gl=angle", "--use-angle=swiftshader"]);
    }
    match platform {
        Platform::Wayland => session.launch_isolated(&program, &args),
        Platform::X11 => session.launch_x11_isolated(&program, &args),
    }
    .unwrap();
    let mut last_error = String::new();
    let mut browser = poll_until(
        Duration::from_secs(60),
        "the private browser's exact test page",
        || {
            if let Some(status) = session.client_status(&program).unwrap() {
                panic!(
                    "test browser exited {status}: {}",
                    session.client_log(&program)
                );
            }
            match Browser::connect(&profile, &url) {
                Ok(browser) => Some(browser),
                Err(error) => {
                    last_error = error;
                    None
                }
            }
        },
    )
    .unwrap_or_else(|error| panic!("{error}: {last_error}\n{}", session.client_log(&program)));
    poll_until(
        EVENT,
        "the test page's DOM fixture to finish loading",
        || {
            browser
                .evaluate("typeof window.chonkProbe === 'object'")
                .ok()?
                .as_bool()?
                .then_some(())
        },
    )
    .unwrap();
    session
        .wait_for_window("ChonkStep Selection Probe")
        .unwrap();
    poll_until(EVENT, "the browser to adopt the compositor's scale", || {
        let viewport = value(&mut browser);
        ((coordinate(&viewport, "dpr") - f64::from(scale)).abs() < 0.01).then_some(())
    })
    .unwrap();
    let version = browser.call("Browser.getVersion", json!({})).unwrap();
    std::fs::write(
        session.dir.join("browser-metadata.json"),
        serde_json::to_vec_pretty(
            &json!({"program":program, "scale":scale, "backend":platform.name(), "version":version}),
        )
        .unwrap(),
    )
    .unwrap();
    for phase in ["windowed", "fullscreen"] {
        if phase == "fullscreen" {
            browser
                .evaluate("document.documentElement.requestFullscreen().then(() => true)")
                .unwrap();
            poll_until(
                EVENT,
                "the page and compositor to agree on fullscreen extent",
                || {
                    let observed = value(&mut browser);
                    let dpr = coordinate(&observed, "dpr");
                    let world = session.world().ok()?;
                    let client = world.window_matching("ChonkStep Selection Probe")?;
                    (observed["fullscreen"] == true
                        && client.w == world.output_w
                        && client.h == world.output_h
                        // XWayland's configure and the browser renderer's
                        // resize are asynchronous. Fullscreen state alone
                        // does not mean the DOM has its new viewport yet.
                        && (coordinate(&observed, "width") * dpr - f64::from(client.w)).abs() <= dpr
                        && (coordinate(&observed, "height") * dpr - f64::from(client.h)).abs() <= dpr)
                        .then_some(())
                },
            )
            .unwrap();
        }
        // A configure updates compositor and DOM geometry before Chromium's
        // newly sized Wayland buffer necessarily reaches the scene. Fence the
        // client's actual commit, then fence the compositor frame that draws
        // it. This prevents screenshots from racing a correctly asynchronous
        // fullscreen transition while preserving the old buffer as evidence
        // on a real timeout.
        wait_for_presented_extent(&mut session, phase);
        session.door().barrier().unwrap();
        for id in ["plain", "editor"] {
            selection(&mut session, &mut browser, phase, id);
        }
        held_arrow(&mut session, &mut browser, phase);
        for id in ["input", "textarea"] {
            field_repeat(&mut session, &mut browser, phase, id);
        }
        if phase == "windowed" {
            modal_repeat(&mut session, &mut browser);
        }
    }
    let _ = browser.call("Browser.close", json!({}));
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn browser_text_selection_and_repeat_at_one_times_scale() {
    run("browser-selection-1", 1.0, Platform::Wayland);
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn browser_text_selection_and_repeat_at_fractional_scale() {
    run("browser-selection-1-5", 1.5, Platform::Wayland);
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn browser_text_selection_and_repeat_at_two_times_scale() {
    run("browser-selection-2", 2.0, Platform::Wayland);
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn xwayland_browser_text_selection_and_repeat_at_one_times_scale() {
    run("browser-selection-x11-1", 1.0, Platform::X11);
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn xwayland_browser_text_selection_and_repeat_at_fractional_scale() {
    run("browser-selection-x11-1-5", 1.5, Platform::X11);
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn xwayland_browser_text_selection_and_repeat_at_two_times_scale() {
    run("browser-selection-x11-2", 2.0, Platform::X11);
}
