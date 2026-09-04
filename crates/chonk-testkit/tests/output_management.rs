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
