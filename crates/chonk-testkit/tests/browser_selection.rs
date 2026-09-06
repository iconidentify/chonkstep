//! Real Chromium-family text selection and key repeat. Override
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
    let origin = (
        f64::from(client.x) + f64::from(client.w) - coordinate(&viewport, "width") * dpr,
        f64::from(client.y) + f64::from(client.h) - coordinate(&viewport, "height") * dpr,
    );
    let global = |point: &Value| {
        (
            origin.0 + coordinate(point, "x") * dpr,
            origin.1 + coordinate(point, "y") * dpr,
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

fn held_arrow(session: &mut Session, browser: &mut Browser) {
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
        session.dir.join("keyboard-repeat.json"),
        serde_json::to_vec_pretty(&json!({"repeated":repeated, "released":released})).unwrap(),
    )
    .unwrap();
}

fn run(name: &str, scale: f32) {
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
            config_extra: "show_dock = false\nomarchy_menu = false\nhyprland_config = false\n"
                .into(),
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
    let mut args = vec![
        "--ozone-platform=wayland",
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
    if std::env::var_os("CI").is_some() {
        // Same private-page software-renderer policy as the existing Chromium
        // regressions on hosted runners that disable unprivileged namespaces.
        args.extend(["--no-sandbox", "--use-gl=angle", "--use-angle=swiftshader"]);
    }
    session.launch_isolated(&program, &args).unwrap();
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
            &json!({"program":program, "scale":scale, "backend":"wayland", "version":version}),
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
                    let world = session.world().ok()?;
                    let client = world.window_matching("ChonkStep Selection Probe")?;
                    (observed["fullscreen"] == true
                        && client.w == world.output_w
                        && client.h == world.output_h)
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
    }
    held_arrow(&mut session, &mut browser);
    let _ = browser.call("Browser.close", json!({}));
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn browser_text_selection_and_repeat_at_one_times_scale() {
    run("browser-selection-1", 1.0);
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn browser_text_selection_and_repeat_at_fractional_scale() {
    run("browser-selection-1-5", 1.5);
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn browser_text_selection_and_repeat_at_two_times_scale() {
    run("browser-selection-2", 2.0);
}
