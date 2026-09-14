//! `wlr-output-management` against the real command-line client.
//!
//! Registry enumeration proves only that the global can be bound. This
//! test makes `wlr-randr` consume a complete head/mode publication,
//! applies a fractional scale, reads that state back through a second
//! manager, and checks that an unsupported disable is rejected without
//! costing the desktop. Those are the three distinct server paths:
//! announce, apply/update, and fail.
//!
//! # Why the plain listing rather than `--json`
//!
//! This test asked for `--json` and had never once run: `wlr-randr`
//! was not installed on CI, and the skip guard reported `ok` in 0.00s
//! (see #75). The first run with the package installed found that
//! Ubuntu ships a `wlr-randr` predating that flag — `unrecognized
//! option '--json'` — so the JSON shape was never a property of the
//! client this suite can actually get.
//!
//! The default listing is the interface every version has had, and it
//! is what a user sees, so it is what is parsed here. The two readers
//! below take only the two facts the assertions need and are
//! deliberately tolerant of the surrounding format: the head name is
//! the first token of an unindented line, and the scale is whatever
//! follows `Scale:`.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use chonk_testkit::{poll_until, Session, SessionOptions};

const CLIENT: &str = "wlr-randr";
const WAIT: Duration = Duration::from_secs(10);

fn run(session: &mut Session, args: &[&str]) -> Result<String, String> {
    session.launch(CLIENT, args)?;
    let status = poll_until(WAIT, "wlr-randr to finish", || {
        session.client_status(CLIENT).ok().flatten()
    })?;
    let report = session.client_log(CLIENT);
    if status.success() {
        Ok(report)
    } else {
        Err(format!("wlr-randr exited with {status}: {report}"))
    }
}

/// The head names in a listing: `wlr-randr` prints each head flush
/// left and indents every property under it, so an unindented,
/// non-empty line begins a head and its first token is the name.
fn head_names(report: &str) -> Vec<&str> {
    report
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with(char::is_whitespace))
        .filter_map(|line| line.split_whitespace().next())
        .collect()
}

/// The scale a listing reports, from the `Scale: 1.500000` property.
/// Parsed as a float rather than string-matched because the number of
/// decimal places is the client's business, not this test's.
fn reported_scale(report: &str) -> Option<f64> {
    report
        .lines()
        .find_map(|line| line.trim().strip_prefix("Scale:"))
        .and_then(|value| value.trim().parse().ok())
}

#[test]
#[ignore = "needs a live Wayland session and wlr-randr"]
fn wlr_randr_lists_applies_and_observes_output_state() {
    if !chonk_testkit::require_client(CLIENT) {
        return;
    }

    let mut session = Session::boot("output-management", SessionOptions::default()).unwrap();
    // Announce: a complete head publication the client can consume.
    let initial = run(&mut session, &[]).expect("initial output listing succeeds");
    assert!(
        head_names(&initial).contains(&"chonkstep"),
        "the compositor must announce its head by name: {initial:?}"
    );
    assert_eq!(reported_scale(&initial), Some(1.0), "a fresh session is at scale 1: {initial:?}");

    // Apply, then read it back through a second manager — the point of
    // re-running the client rather than trusting the first one's exit
    // code.
    run(&mut session, &["--output", "chonkstep", "--scale", "1.5"])
        .expect("fractional output scale applies");
    let updated = run(&mut session, &[]).expect("updated output listing succeeds");
    assert!(
        head_names(&updated).contains(&"chonkstep"),
        "the head must still be announced after a change: {updated:?}"
    );
    assert_eq!(
        reported_scale(&updated),
        Some(1.5),
        "the applied fractional scale must come back through a fresh manager: {updated:?}"
    );

    session.launch(CLIENT, &["--output", "chonkstep", "--off"]).unwrap();
    let status = poll_until(WAIT, "unsupported output disable to be answered", || {
        session.client_status(CLIENT).ok().flatten()
    })
    .unwrap();
    assert!(!status.success(), "wlr-randr must report the compositor's refusal");
    assert!(
        session.client_log(CLIENT).contains("failed"),
        "the client should explain that the configuration failed"
    );
    assert!(session.compositor_alive(), "a refused configuration keeps the desktop alive");
}

/// One JSON reply from the session's Hyprland request socket.
fn hyprland_json(session: &Session, request: &str) -> serde_json::Value {
    let log = session.log();
    let directory = log
        .lines()
        .find(|line| line.contains("hyprland ipc listening"))
        .and_then(|line| line.split("directory=\"").nth(1)?.split('"').next())
        .expect("the compositor announces its Hyprland IPC directory")
        .to_string();
    let mut socket = UnixStream::connect(std::path::Path::new(&directory).join(".socket.sock")).unwrap();
    socket.set_read_timeout(Some(WAIT)).unwrap();
    socket.write_all(request.as_bytes()).unwrap();
    socket.shutdown(std::net::Shutdown::Write).unwrap();
    let mut response = String::new();
    socket.read_to_string(&mut response).unwrap();
    serde_json::from_str(&response).unwrap_or_else(|error| panic!("{request}: {error}: {response}"))
}

/// Whether the live keymap holds a binding on `key` (any modifiers).
fn bound(session: &Session, key: &str) -> bool {
    hyprland_json(session, "j/binds")
        .as_array()
        .expect("binds is an array")
        .iter()
        .any(|bind| bind["key"].as_str().is_some_and(|name| name.eq_ignore_ascii_case(key)))
}

/// A primary-scale change through wlr-output-management restyles the
/// running session. It is not a reload: a `config.toml` that stopped
/// parsing must not replace the user's bindings with the defaults, and a
/// valid edit nobody reloaded must wait for the reload.
#[test]
#[ignore = "needs a live Wayland session and wlr-randr"]
fn a_wlr_randr_scale_change_keeps_the_running_configuration() {
    if !chonk_testkit::require_client(CLIENT) {
        return;
    }
    let options = SessionOptions {
        config_extra: "[keybindings]\n\"super+f9\" = \"toggle-fullscreen\"\n".into(),
        ..SessionOptions::default()
    };
    let mut session = Session::boot("output-management-live-config", options).unwrap();
    assert!(bound(&session, "F9"), "the configured binding is live at boot");

    session.rewrite_config("this = = is not [ toml\n").unwrap();
    run(&mut session, &["--output", "chonkstep", "--scale", "1.5"]).expect("the scale applies");
    let listing = run(&mut session, &[]).expect("listing succeeds");
    assert_eq!(reported_scale(&listing), Some(1.5), "{listing:?}");
    assert!(bound(&session, "F9"), "a file that no longer parses must not reset the bindings");

    session
        .rewrite_config("[keybindings]\n\"super+f10\" = \"toggle-fullscreen\"\n")
        .unwrap();
    run(&mut session, &["--output", "chonkstep", "--scale", "2"]).expect("the second scale applies");
    assert!(bound(&session, "F9") && !bound(&session, "F10"), "an unreloaded edit stays out");

    session.request_reload().unwrap();
    poll_until(WAIT, "the reload to apply the edited binding", || bound(&session, "F10").then_some(()))
        .expect("an explicit reload applies the edit");
}

/// Plugging a monitor places it by the monitor rules the running session
/// already holds. A `config.toml` that no longer parses used to drop
/// `desktop = "omarchy"`, and with it every rule read from the Hyprland
/// files, so the new head came up at the automatic scale.
#[test]
#[ignore = "needs a live Wayland session"]
fn a_plugged_monitor_takes_its_running_rule_when_the_file_no_longer_parses() {
    let options = SessionOptions {
        config_extra: "desktop = \"omarchy\"\nomarchy_bar = false\nshow_dock = false\n".into(),
        config_root_files: vec![(
            "hypr/hyprland.conf".into(),
            "monitor = chonkstep-right, preferred, auto, 2\nbind = SUPER, F12, workspace, 1\n".into(),
        )],
        ..SessionOptions::default()
    };
    let mut session = Session::boot("output-management-hotplug-rules", options).unwrap();
    session.rewrite_config("this = = is not [ toml\n").unwrap();
    session.door().set_virtual_outputs("split").unwrap();
    let scale = poll_until(WAIT, "the plugged head to be reported", || {
        let monitors = hyprland_json(&session, "j/monitors");
        let right = monitors.as_array()?.iter().find(|monitor| monitor["name"] == "chonkstep-right")?;
        right["scale"].as_f64()
    })
    .unwrap();
    assert_eq!(scale, 2.0, "chonkstep-right must take its monitor rule's scale");
    assert!(session.compositor_alive());
}

/// A listing in the shape `wlr-randr` prints, so the two readers above
/// are pinned without needing the client installed. Taken from the
/// format `wlr-randr` has emitted since it grew scale reporting: each
/// head flush left with its description quoted, every property
/// indented beneath it, and the modes indented one level further.
const SAMPLE_LISTING: &str = "\
chonkstep \"chonkstep chonkstep Unknown\"
  Make: chonkstep
  Model: chonkstep
  Serial: Unknown
  Physical size: 0x0 mm
  Enabled: yes
  Modes:
    2560x1600 px, 60.000000 Hz (preferred, current)
  Position: 0,0
  Transform: normal
  Scale: 1.500000
";

#[test]
fn a_listing_yields_its_head_names_and_not_its_properties() {
    // The indentation is the whole distinction: `Make:` and the mode
    // line must not be mistaken for heads, or the name assertion
    // passes on anything.
    assert_eq!(head_names(SAMPLE_LISTING), vec!["chonkstep"]);
    assert_eq!(head_names(""), Vec::<&str>::new());
    // A second head is a second unindented line.
    let two = format!("{SAMPLE_LISTING}HDMI-A-1 \"Other\"\n  Scale: 1.000000\n");
    assert_eq!(head_names(&two), vec!["chonkstep", "HDMI-A-1"]);
}

#[test]
fn a_listing_yields_its_scale_as_a_number() {
    // Parsed, not string-matched: how many decimal places the client
    // prints is its business, and an assertion on the text would break
    // on a version that printed `1.5`.
    assert_eq!(reported_scale(SAMPLE_LISTING), Some(1.5));
    assert_eq!(reported_scale("  Scale: 1\n"), Some(1.0));
    assert_eq!(reported_scale("  Position: 0,0\n"), None);
    assert_eq!(reported_scale(""), None);
}
