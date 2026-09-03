//! Canned `bluetoothctl` transcripts, folded line by line.
//!
//! **None of this has run against a Bluetooth adapter.** The machine
//! this was written on has no controller — `/sys/class/bluetooth` does
//! not exist, `bluetooth.service` is inactive, and every `bluetoothctl`
//! subcommand hangs until killed — so the transcripts below are
//! written from the tool's documented and observed output shapes at
//! version 5.87, not captured from a live pairing. They pin the fold
//! exactly; they are not evidence that pairing works on any particular
//! headset, and the two claims should not be confused.

use super::*;

/// Everything a transcript makes the machine want to say back.
fn sends(steps: &[Step]) -> Vec<String> {
    steps
        .iter()
        .filter_map(|step| match step {
            Step::Send(line) => Some(line.clone()),
            Step::Repaint => None,
        })
        .collect()
}

/// Feeds a whole transcript, returning every command it produced.
fn feed(pairing: &mut Pairing, transcript: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in transcript.lines() {
        out.extend(sends(&pairing.on_line(line)));
    }
    out
}

/// A machine that has answered the opening handshake and is scanning.
fn scanning() -> Pairing {
    let mut pairing = Pairing::new();
    assert_eq!(sends(&pairing.opening()), vec!["agent DisplayYesNo", "default-agent"]);
    let started = feed(&mut pairing, "Agent registered\n");
    assert_eq!(started, vec!["scan on"], "discovery waits until there is an agent to answer with");
    assert_eq!(*pairing.phase(), Phase::Scanning);
    pairing
}

#[test]
fn the_opening_registers_an_agent_this_window_can_actually_honor() {
    let pairing = Pairing::new();
    assert_eq!(*pairing.phase(), Phase::Starting);
    assert_eq!(
        sends(&pairing.opening()),
        vec![format!("agent {AGENT_CAPABILITY}"), "default-agent".to_string()],
        "a window with a screen and two buttons and no keyboard is exactly DisplayYesNo"
    );
    assert_eq!(AGENT_CAPABILITY, "DisplayYesNo");
}

// -- discovery ---------------------------------------------------------

#[test]
fn discovery_builds_a_list_and_keeps_it_stable() {
    let mut pairing = scanning();
    feed(
        &mut pairing,
        "[NEW] Device F8:4E:17:00:11:22 WH-1000XM4\n\
         [NEW] Device AA:BB:CC:DD:EE:FF MX Keys\n",
    );
    let devices = pairing.devices();
    assert_eq!(devices.len(), 2);
    // Ordered by address, so the row under the pointer does not move
    // when BlueZ re-announces something mid-scan.
    assert_eq!(devices[0].address, "AA:BB:CC:DD:EE:FF");
    assert_eq!(devices[0].name, "MX Keys");
    assert_eq!(devices[1].name, "WH-1000XM4");
}

#[test]
fn a_re_announced_device_updates_rather_than_duplicating() {
    let mut pairing = scanning();
    feed(
        &mut pairing,
        "[NEW] Device F8:4E:17:00:11:22 F8-4E-17-00-11-22\n\
         [CHG] Device F8:4E:17:00:11:22 WH-1000XM4\n",
    );
    assert_eq!(pairing.devices().len(), 1, "one device is one row");
    assert_eq!(pairing.devices()[0].name, "WH-1000XM4", "the later, better name wins");
}

/// The trap in `[CHG]` lines: most of them carry a property, not a
/// name, and folding `RSSI:` in as a name would rename the device to
/// its signal strength.
#[test]
fn property_updates_never_overwrite_a_name() {
    let mut pairing = scanning();
    feed(
        &mut pairing,
        "[NEW] Device F8:4E:17:00:11:22 WH-1000XM4\n\
         [CHG] Device F8:4E:17:00:11:22 RSSI: -60\n\
         [CHG] Device F8:4E:17:00:11:22 TxPower: 12\n\
         [CHG] Device F8:4E:17:00:11:22 ServicesResolved: yes\n\
         [CHG] Device F8:4E:17:00:11:22 Connected: yes\n",
    );
    assert_eq!(pairing.devices()[0].name, "WH-1000XM4");
}

#[test]
fn a_paired_flag_is_recorded_without_touching_the_name() {
    let mut pairing = scanning();
    feed(
        &mut pairing,
        "[NEW] Device F8:4E:17:00:11:22 WH-1000XM4\n\
         [CHG] Device F8:4E:17:00:11:22 Paired: yes\n",
    );
    assert!(pairing.devices()[0].paired);
    assert_eq!(pairing.devices()[0].name, "WH-1000XM4");
}

#[test]
fn a_device_going_away_leaves_the_list() {
    let mut pairing = scanning();
    feed(&mut pairing, "[NEW] Device F8:4E:17:00:11:22 Buds\n[DEL] Device F8:4E:17:00:11:22 Buds\n");
    assert!(pairing.devices().is_empty());
}

/// A controller line is not a device line, and neither is a device
/// whose address is not a MAC.
#[test]
fn non_device_lines_are_ignored() {
    let mut pairing = scanning();
    feed(
        &mut pairing,
        "[CHG] Controller 00:1A:7D:DA:71:13 Discovering: yes\n\
         [NEW] Device NOT-A-MAC Something\n\
         Discovery started\n\
         [bluetooth]# \n",
    );
    assert!(pairing.devices().is_empty());
    assert_eq!(*pairing.phase(), Phase::Scanning, "noise must not move the machine");
}

/// `bluetoothctl` colors its tags and prints its prompt on the same
/// stream; neither is content, and both land mid-line on a pipe.
#[test]
fn ansi_colored_and_prompt_prefixed_lines_still_parse() {
    let mut pairing = scanning();
    feed(
        &mut pairing,
        "\u{1b}[0;92m[NEW]\u{1b}[0m Device F8:4E:17:00:11:22 WH-1000XM4\r\n\
         \u{1b}[0;94m[bluetooth]\u{1b}[0m# \u{1b}[0;93m[CHG]\u{1b}[0m Device F8:4E:17:00:11:22 Paired: yes\r\n",
    );
    assert_eq!(pairing.devices().len(), 1);
    assert_eq!(pairing.devices()[0].name, "WH-1000XM4");
    assert!(pairing.devices()[0].paired);
}

// -- the happy path ----------------------------------------------------

/// Numeric comparison end to end: the modern default for anything with
/// a display, and the exact reason the agent is `DisplayYesNo`.
#[test]
fn the_numeric_comparison_path_pairs_trusts_and_connects() {
    let mut pairing = scanning();
    feed(&mut pairing, "[NEW] Device F8:4E:17:00:11:22 WH-1000XM4\n");

    // The click. Scanning stops first — it is noise on the same radio.
    let clicked = sends(&pairing.pair_with("F8:4E:17:00:11:22"));
    assert_eq!(clicked, vec!["scan off", "pair F8:4E:17:00:11:22"]);
    assert_eq!(*pairing.phase(), Phase::Pairing { address: "F8:4E:17:00:11:22".to_string() });

    // BlueZ asks. The prompt carries no address, so the machine uses
    // the one it asked to pair with.
    let asked = feed(&mut pairing, "[agent] Confirm passkey 123456 (yes/no): \n");
    assert!(asked.is_empty(), "a question produces no command, only a repaint");
    assert_eq!(
        *pairing.phase(),
        Phase::Confirm { address: "F8:4E:17:00:11:22".to_string(), passkey: "123456".to_string() }
    );

    // The human says yes.
    assert_eq!(sends(&pairing.answer(true)), vec!["yes"]);
    assert_eq!(*pairing.phase(), Phase::Pairing { address: "F8:4E:17:00:11:22".to_string() });

    // And it lands. Trust so it reconnects on its own; connect so the
    // headset someone just paired is not silent.
    let done = feed(&mut pairing, "[CHG] Device F8:4E:17:00:11:22 Paired: yes\nPairing successful\n");
    assert_eq!(done, vec!["trust F8:4E:17:00:11:22", "connect F8:4E:17:00:11:22"]);
    assert_eq!(*pairing.phase(), Phase::Paired { address: "F8:4E:17:00:11:22".to_string() });
}

/// A keyboard is paired the other way round: BlueZ shows the passkey
/// here and the human types it over there. Nothing to answer.
#[test]
fn the_display_passkey_path_shows_the_digits_and_asks_nothing() {
    let mut pairing = scanning();
    pairing.pair_with("AA:BB:CC:DD:EE:FF");
    let steps = feed(&mut pairing, "[agent] Passkey: 042318\n");
    assert!(steps.is_empty());
    assert_eq!(
        *pairing.phase(),
        Phase::DisplayPasskey { address: "AA:BB:CC:DD:EE:FF".to_string(), passkey: "042318".to_string() },
        "a leading zero is part of the passkey, not a number to be trimmed"
    );
    feed(&mut pairing, "Pairing successful\n");
    assert_eq!(*pairing.phase(), Phase::Paired { address: "AA:BB:CC:DD:EE:FF".to_string() });
}

// -- refusals and failures ---------------------------------------------

#[test]
fn saying_no_declines_and_says_who_declined() {
    let mut pairing = scanning();
    pairing.pair_with("F8:4E:17:00:11:22");
    feed(&mut pairing, "[agent] Confirm passkey 999111 (yes/no): \n");
    assert_eq!(sends(&pairing.answer(false)), vec!["no"]);
    match pairing.phase() {
        Phase::Failed { address, reason } => {
            assert_eq!(address, "F8:4E:17:00:11:22");
            assert_eq!(reason, "declined here", "the honest reason is that we declined, not that it failed");
        }
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[test]
fn a_bluez_failure_is_shown_by_its_last_component() {
    let mut pairing = scanning();
    pairing.pair_with("F8:4E:17:00:11:22");
    feed(&mut pairing, "Failed to pair: org.bluez.Error.AuthenticationFailed\n");
    assert_eq!(
        *pairing.phase(),
        Phase::Failed { address: "F8:4E:17:00:11:22".to_string(), reason: "AuthenticationFailed".to_string() }
    );
}

#[test]
fn a_non_bluez_failure_is_shown_whole() {
    let mut pairing = scanning();
    pairing.pair_with("F8:4E:17:00:11:22");
    feed(&mut pairing, "Failed to pair: Device is not available\n");
    match pairing.phase() {
        Phase::Failed { reason, .. } => assert_eq!(reason, "Device is not available"),
        other => panic!("expected a failure, got {other:?}"),
    }
}

/// The capability mismatch, named rather than hung on. A legacy device
/// asking this agent to *supply* a PIN is asking for something a window
/// with no keyboard cannot give.
#[test]
fn a_pin_request_is_refused_honestly_rather_than_waited_on() {
    for prompt in ["[agent] Enter PIN code: ", "[agent] Request PIN code", "[agent] Request passkey"] {
        let mut pairing = scanning();
        pairing.pair_with("AA:BB:CC:DD:EE:FF");
        feed(&mut pairing, prompt);
        assert_eq!(
            *pairing.phase(),
            Phase::NeedsKeyboard { address: "AA:BB:CC:DD:EE:FF".to_string() },
            "{prompt:?} must not leave the dialog sitting on a prompt it cannot answer"
        );
    }
}

/// The development host's own answer, and the one this dialog will
/// give on every machine without a controller.
#[test]
fn no_controller_is_terminal_and_says_so() {
    let mut pairing = Pairing::new();
    feed(&mut pairing, "No default controller available\n");
    assert_eq!(*pairing.phase(), Phase::Unavailable { reason: "no Bluetooth controller".to_string() });
}

#[test]
fn a_dead_daemon_is_terminal_and_distinguishable() {
    let mut pairing = Pairing::new();
    feed(&mut pairing, "Waiting to connect to bluetoothd...\n");
    assert_eq!(*pairing.phase(), Phase::Unavailable { reason: "bluetoothd is not running".to_string() });
}

/// A terminal condition outranks whatever the machine was doing: none
/// of the rest can succeed after the controller has gone.
#[test]
fn losing_the_controller_mid_pair_is_still_terminal() {
    let mut pairing = scanning();
    pairing.pair_with("F8:4E:17:00:11:22");
    feed(&mut pairing, "No default controller available\n");
    assert!(matches!(pairing.phase(), Phase::Unavailable { .. }));
}

// -- the machine's own guards ------------------------------------------

#[test]
fn only_one_pairing_at_a_time() {
    let mut pairing = scanning();
    assert!(!pairing.pair_with("F8:4E:17:00:11:22").is_empty());
    assert!(
        pairing.pair_with("AA:BB:CC:DD:EE:FF").is_empty(),
        "a second pair mid-negotiation is how a pairing gets confused"
    );
    assert_eq!(*pairing.phase(), Phase::Pairing { address: "F8:4E:17:00:11:22".to_string() });
}

#[test]
fn answering_when_nothing_was_asked_does_nothing() {
    let mut pairing = scanning();
    assert!(pairing.answer(true).is_empty());
    assert!(pairing.answer(false).is_empty());
    assert_eq!(*pairing.phase(), Phase::Scanning);
}

#[test]
fn rescanning_clears_the_list_and_starts_over() {
    let mut pairing = scanning();
    feed(&mut pairing, "[NEW] Device F8:4E:17:00:11:22 Buds\n");
    pairing.pair_with("F8:4E:17:00:11:22");
    feed(&mut pairing, "Failed to pair: org.bluez.Error.AuthenticationFailed\n");

    assert_eq!(sends(&pairing.rescan()), vec!["scan on"]);
    assert_eq!(*pairing.phase(), Phase::Scanning);
    assert!(pairing.devices().is_empty(), "a fresh scan starts from a fresh list");
}

/// The adapter must not be left discovering after the window closes.
#[test]
fn closing_stops_the_scan_before_it_quits() {
    let pairing = scanning();
    assert_eq!(sends(&pairing.closing()), vec!["scan off", "quit"]);
}

/// The whole session as one transcript, which is the shape a reviewer
/// can check against a real `bluetoothctl` log by eye.
#[test]
fn a_whole_session_folds_to_the_expected_command_stream() {
    let mut pairing = Pairing::new();
    let mut issued = sends(&pairing.opening());
    issued.extend(feed(
        &mut pairing,
        "Agent registered\n\
         [CHG] Controller 00:1A:7D:DA:71:13 Discovering: yes\n\
         [NEW] Device F8:4E:17:00:11:22 WH-1000XM4\n\
         [CHG] Device F8:4E:17:00:11:22 RSSI: -54\n",
    ));
    issued.extend(sends(&pairing.pair_with("F8:4E:17:00:11:22")));
    issued.extend(feed(&mut pairing, "[agent] Confirm passkey 123456 (yes/no): \n"));
    issued.extend(sends(&pairing.answer(true)));
    issued.extend(feed(&mut pairing, "Pairing successful\n"));
    issued.extend(sends(&pairing.closing()));

    assert_eq!(
        issued,
        vec![
            "agent DisplayYesNo",
            "default-agent",
            "scan on",
            "scan off",
            "pair F8:4E:17:00:11:22",
            "yes",
            "trust F8:4E:17:00:11:22",
            "connect F8:4E:17:00:11:22",
            "scan off",
            "quit",
        ]
    );
}
