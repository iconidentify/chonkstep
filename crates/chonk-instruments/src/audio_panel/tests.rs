//! Audio panel unit tests: the parse over canned `pactl` output
//! (shaped from this machine's real captures, escapes included), the
//! action argv tables, the press/release/prediction state machine, and
//! the renderer's byte-stable states.

use super::*;
use render::render_audio_panel;
use wm_theme::default_theme::{all_themes, nextstep_classic};

/// Trimmed from a real `pactl --format=json list sinks` capture
/// (2026-09-02, PipeWire): three sinks — an available HDMI at 40%, the
/// RUNNING Volt 4 interface at 100%, and a third whose only port is
/// physically absent.
const SINKS_JSON: &str = r#"[
  {
    "index": 66,
    "state": "SUSPENDED",
    "name": "alsa_output.pci-0000_01_00.1.hdmi-stereo",
    "description": "GA102 High Definition Audio Controller Digital Stereo (HDMI)",
    "driver": "PipeWire",
    "mute": false,
    "volume": {
      "front-left": { "value": 26214, "value_percent": "40%", "db": "-23.88 dB" },
      "front-right": { "value": 26214, "value_percent": "40%", "db": "-23.88 dB" }
    },
    "balance": 0.0,
    "properties": { "iec958.codecs": "[\"PCM\"]", "device.bus": "pci" },
    "ports": [
      { "name": "hdmi-output-0", "description": "HDMI / DisplayPort", "type": "HDMI", "availability_group": "Legacy 4", "availability": "available" }
    ],
    "active_port": "hdmi-output-0"
  },
  {
    "index": 68,
    "state": "RUNNING",
    "name": "alsa_output.usb-Universal_Audio_Volt_4_22332055008061-00.analog-surround-40",
    "description": "Volt 4 Analog Surround 4.0",
    "driver": "PipeWire",
    "mute": false,
    "volume": {
      "front-left": { "value": 65536, "value_percent": "100%", "db": "0.00 dB" },
      "front-right": { "value": 65536, "value_percent": "100%", "db": "0.00 dB" },
      "rear-left": { "value": 65536, "value_percent": "100%", "db": "0.00 dB" },
      "rear-right": { "value": 65536, "value_percent": "100%", "db": "0.00 dB" }
    },
    "properties": { "device.api": "alsa" },
    "ports": [
      { "name": "analog-output", "description": "Analog Output", "type": "Analog", "availability_group": "Legacy 1", "availability": "availability unknown" }
    ],
    "active_port": "analog-output"
  },
  {
    "index": 70,
    "state": "SUSPENDED",
    "name": "alsa_output.pci-0000_00_1f.3.iec958-stereo",
    "description": "Built-in Audio Digital Stereo (IEC958)",
    "mute": true,
    "volume": {
      "front-left": { "value": 26214, "value_percent": "40%", "db": "-23.88 dB" }
    },
    "ports": [
      { "name": "iec958-stereo-output", "description": "Digital Output (S/PDIF)", "type": "SPDIF", "availability": "not available" }
    ],
    "active_port": "iec958-stereo-output"
  }
]"#;

/// Trimmed from the matching `pactl --format=json list sink-inputs`
/// capture: one real application stream — with the double-escaped
/// `format` field `pactl` really emits — plus a nameless DSP stream
/// and an EasyEffects output, both of which a switch must leave alone.
const SINK_INPUTS_JSON: &str = r#"[
  {
    "index": 19288,
    "driver": "PipeWire",
    "owner_module": null,
    "sink": 68,
    "format": "pcm, format.sample_format = \"\\\"float32le\\\"\"  format.rate = \"48000\"",
    "corked": false,
    "mute": false,
    "properties": { "application.name": "Microsoft Edge", "application.process.binary": "msedge" }
  },
  {
    "index": 301,
    "sink": 68,
    "properties": { "media.name": "output_FL" }
  },
  {
    "index": 302,
    "sink": 68,
    "properties": { "application.name": "EasyEffects" }
  }
]"#;

const HDMI: &str = "alsa_output.pci-0000_01_00.1.hdmi-stereo";
const VOLT: &str = "alsa_output.usb-Universal_Audio_Volt_4_22332055008061-00.analog-surround-40";
const SPDIF: &str = "alsa_output.pci-0000_00_1f.3.iec958-stereo";

fn fixture_sinks() -> Vec<AudioSink> {
    parse_sinks(SINKS_JSON).expect("the canned capture parses")
}

// -----------------------------------------------------------------
// Parsing
// -----------------------------------------------------------------

#[test]
fn parses_the_real_sink_capture_field_for_field() {
    let sinks = fixture_sinks();
    assert_eq!(sinks.len(), 3);

    assert_eq!(sinks[0].name, HDMI);
    assert_eq!(sinks[0].description, "GA102 High Definition Audio Controller Digital Stereo (HDMI)");
    assert!(!sinks[0].muted);
    assert_eq!(sinks[0].volume_percent, Some(40));
    assert!(sinks[0].available);
    assert!(!sinks[0].running);

    assert_eq!(sinks[1].name, VOLT);
    assert!(sinks[1].running, "RUNNING state must fold to running");
    assert_eq!(sinks[1].volume_percent, Some(100));
    assert!(sinks[1].available, "availability unknown counts as present");

    assert_eq!(sinks[2].name, SPDIF);
    assert!(sinks[2].muted);
    assert!(!sinks[2].available, "a sink whose every port is 'not available' greys out");
}

#[test]
fn sink_edge_cases_degrade_per_field_not_per_document() {
    // No ports at all (a virtual/null sink) is available; a missing
    // description falls back to the name; no volume blanks the readout.
    let doc = r#"[
      { "name": "null.sink", "state": "IDLE", "mute": false, "volume": {}, "ports": [] },
      { "index": 3, "comment": "nameless, dropped" },
      { "name": "combo", "description": "  ", "ports": [ { "availability": "not available" }, { "availability": "available" } ] }
    ]"#;
    let sinks = parse_sinks(doc).unwrap();
    assert_eq!(sinks.len(), 2, "a nameless entry is dropped alone");
    assert_eq!(sinks[0].name, "null.sink");
    assert!(sinks[0].available, "no ports means nothing can be unplugged");
    assert_eq!(sinks[0].volume_percent, None);
    assert_eq!(sinks[0].description, "null.sink", "blank description falls back to the name");
    assert!(sinks[1].available, "one live port is enough");
}

#[test]
fn damaged_sink_documents_are_no_reading() {
    for bad in ["", "not json", "{\"a\": 1}", "[{\"name\": \"x\""] {
        assert_eq!(parse_sinks(bad), None, "{bad:?} must not produce a sink list");
    }
}

#[test]
fn percent_strings_parse_the_pactl_way() {
    assert_eq!(parse_percent("40%"), Some(40));
    assert_eq!(parse_percent("100%"), Some(100));
    assert_eq!(parse_percent("153%"), Some(153), "overdrive passes through");
    assert_eq!(parse_percent(" 40% "), Some(40));
    assert_eq!(parse_percent("40.6%"), Some(41), "a future decimal rounds instead of blanking");
    for bad in ["40", "%", "", "-5%", "loud%"] {
        assert_eq!(parse_percent(bad), None, "{bad:?}");
    }
}

#[test]
fn default_sink_is_one_bare_name_line() {
    assert_eq!(parse_default_sink("alsa_output.foo.analog-stereo\n"), Some("alsa_output.foo.analog-stereo".to_string()));
    assert_eq!(parse_default_sink(""), None);
    assert_eq!(parse_default_sink("   \n"), None);
    assert_eq!(parse_default_sink("No default sink set.\n"), None, "an error sentence is not a name");
}

#[test]
fn sink_inputs_parse_and_the_move_filter_spares_the_dsp_chain() {
    let inputs = parse_sink_inputs(SINK_INPUTS_JSON);
    assert_eq!(inputs.len(), 3);
    assert_eq!(inputs[0], SinkInput { index: 19288, app_name: Some("Microsoft Edge".to_string()) });
    assert_eq!(inputs[1].app_name, None, "a filter-chain stream carries no application.name");
    assert_eq!(eligible_moves(&inputs), vec![19288], "only the real application moves");
    assert!(parse_sink_inputs("garbage").is_empty(), "damage degrades to no moves, not no switch");
}

// -----------------------------------------------------------------
// Action argv
// -----------------------------------------------------------------

#[test]
fn a_switch_is_set_default_plus_migrations_all_by_name() {
    let inputs = parse_sink_inputs(SINK_INPUTS_JSON);
    let confirm = chonk_dock_widget::SourceId::from_index(2);
    let effects = action_effects(&PanelAction::SwitchTo(HDMI.to_string()), &inputs, Some(confirm));
    assert_eq!(effects.len(), 2, "one set-default plus one eligible move");
    match &effects[0] {
        Effect::Run { program, args, then } => {
            assert_eq!(*program, "pactl");
            assert_eq!(args, &["set-default-sink", HDMI]);
            assert_eq!(*then, Some(confirm), "the default sampler confirms the switch");
        }
        _ => panic!("a switch must run pactl"),
    }
    match &effects[1] {
        Effect::Run { program, args, then } => {
            assert_eq!(*program, "pactl");
            assert_eq!(args, &["move-sink-input", "19288", HDMI]);
            assert_eq!(*then, None, "migrations change nothing the panel draws");
        }
        _ => panic!("a migration must run pactl"),
    }
}

#[test]
fn a_level_set_is_absolute_and_by_name() {
    let effects = action_effects(&PanelAction::SetVolume { sink: VOLT.to_string(), percent: 45 }, &[], None);
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::Run { program, args, .. } => {
            assert_eq!(*program, "pactl");
            assert_eq!(args, &["set-sink-volume", VOLT, "45%"]);
        }
        _ => panic!("a level set must run pactl"),
    }
}

#[test]
fn a_mute_toggle_is_one_command_on_one_name() {
    let effects = action_effects(&PanelAction::ToggleMute(VOLT.to_string()), &[], None);
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::Run { program, args, .. } => {
            assert_eq!(*program, "pactl");
            assert_eq!(args, &["set-sink-mute", VOLT, "toggle"]);
        }
        _ => panic!("a mute toggle must run pactl"),
    }
}

// -----------------------------------------------------------------
// Geometry
// -----------------------------------------------------------------

use wm_theme_api::Point;

#[test]
fn the_request_scales_with_the_tile() {
    // 392 wide (seven tiles — wide enough that a PulseAudio
    // description reaches its distinguishing tail), and tall enough for
    // the glass inset, the OUTPUTS band, three 32px rows and the
    // grooves between them.
    let m = m56();
    assert_eq!(PanelMetrics::request(56, 3), (392, 3 + 4 + m.header_h + 2 + 3 * 32 + 2 * 2 + 4 + 3));
    assert_eq!(PanelMetrics::request(56, 0), PanelMetrics::request(56, 1), "the empty panel keeps one row of face");

    let (hidpi_w, hidpi_h) = PanelMetrics::request(112, 3);
    assert_eq!(hidpi_w, 784, "the panel doubles with the tile");
    assert!(hidpi_h > PanelMetrics::request(56, 3).1);
}

#[test]
fn a_granted_panel_hit_tests_where_it_draws() {
    let m = m56();
    assert_eq!((m.width, m.row_h, m.pad, m.gap), (392, 32, 4, 2));
    assert_eq!(m.height, PanelMetrics::request(56, 3).1);

    // The row stack starts under the OUTPUTS band, and every row is
    // one row-height of glass with a groove-wide gap after it.
    let top = m.rows_top();
    assert_eq!(m.row_top(0), top);
    assert_eq!(m.row_top(1), top + 34);
    assert_eq!(m.row_at(Point::new(10, top), 3), Some(0));
    assert_eq!(m.row_at(Point::new(10, top + 31), 3), Some(0));
    assert_eq!(m.row_at(Point::new(10, top + 32), 3), None, "the groove belongs to nobody");
    assert_eq!(m.row_at(Point::new(10, top + 34), 3), Some(1));
    assert_eq!(m.row_at(Point::new(10, top - 1), 3), None, "the header is not a row");
    assert_eq!(m.row_at(Point::new(10, m.height as i32 - 1), 3), None, "past the last row is nothing");
    assert_eq!(m.row_at(Point::new(0, top + 10), 3), None, "the gasket is not the panel");
    assert_eq!(m.row_at(Point::new(392, top + 10), 3), None);
    assert_eq!(m.mute_zone_left(), 392 - 3 - 4 - 32, "the mute key sits a pad in from the glass edge");
}

/// The grant, not the request, is the geometry: a narrower panel moves
/// the mute key in, a shorter one compresses the rows to keep every
/// device on the face, and a grant too short even for that hides the
/// tail from the hit-test as well as from the paint.
#[test]
fn a_clamped_grant_is_obeyed_rather_than_overdrawn() {
    let (natural_w, natural_h) = PanelMetrics::request(56, 3);
    let top = m56().rows_top();

    let narrow = PanelMetrics::granted(56, 240, natural_h, 3);
    assert_eq!(narrow.width, 240, "the granted width is the width");
    assert_eq!(narrow.mute_zone_left(), 240 - 3 - 4 - 32, "the mute key follows the right edge in");
    assert_eq!(narrow.row_at(Point::new(236, top + 10), 3), Some(0));
    assert_eq!(narrow.row_at(Point::new(240, top + 10), 3), None, "past the grant is not the panel");

    // Two thirds of the height still shows all three devices, at a
    // compressed row height.
    let short = PanelMetrics::granted(56, natural_w, natural_h * 2 / 3, 3);
    assert!(short.row_h < 32, "rows compress to fit a clamped grant");
    assert_eq!(short.visible_rows(3), 3, "and every device survives the clamp");
    assert!(
        short.row_top(2) + short.row_h as i32 <= (natural_h * 2 / 3) as i32,
        "the last row must land inside the grant"
    );

    // Below the legibility floor the tail is dropped instead — and a
    // dropped row is not clickable.
    let tiny = PanelMetrics::granted(56, natural_w, 72, 3);
    assert_eq!(tiny.row_h, MIN_ROW_H, "compression stops at the legibility floor");
    assert_eq!(tiny.visible_rows(3), 2);
    assert_eq!(tiny.row_at(Point::new(10, tiny.row_top(0) + 2), 3), Some(0));
    assert_eq!(tiny.row_at(Point::new(10, tiny.row_top(2) + 2), 3), None, "a row that is not drawn is not a target");

    // A grant taller than the request is glass, not stretch.
    let tall = PanelMetrics::granted(56, natural_w, natural_h * 3, 3);
    assert_eq!(tall.row_h, 32, "extra height is glass, not fatter rows");
    assert_eq!(tall.height, natural_h * 3);

    // Absurdly narrow: the mute key leaves rather than swallowing the
    // row, so a press still means "make this the default".
    let sliver = PanelMetrics::granted(56, 80, natural_h, 3);
    assert!(
        sliver.mute_zone_left() >= sliver.width as i32,
        "the key leaves the panel rather than eating the row"
    );
    assert_eq!(sliver.row_at(Point::new(40, top + 10), 3), Some(0), "and the body is still the switch target");

    let hairline = PanelMetrics::granted(56, 60, natural_h, 3);
    assert!(hairline.mute_zone_left() >= hairline.width as i32, "narrower still, the mute key is gone too");
}

// -----------------------------------------------------------------
// The interaction state machine
// -----------------------------------------------------------------

/// A panel folded from the canned captures, defaulted to the Volt.
fn fixture_panel() -> AudioPanel {
    let mut panel = AudioPanel::new();
    panel.fold_sinks(parse_sinks(SINKS_JSON));
    panel.fold_default(Some(VOLT.to_string()));
    panel.fold_inputs(parse_sink_inputs(SINK_INPUTS_JSON));
    panel
}

/// The three-device panel granted exactly what it asked for at a 56px
/// tile: 336 wide, 32px rows, 4px pad, 2px gap, under an OUTPUTS band.
fn m56() -> PanelMetrics {
    let (w, h) = PanelMetrics::request(56, 3);
    PanelMetrics::granted(56, w, h, 3)
}

/// A point on row `i`'s body / mute key at the 56px metrics, read off
/// the metrics themselves so a change to the panel's furniture moves
/// the probes with the rows instead of leaving them on the header.
fn on_row(i: i32) -> Point {
    let m = m56();
    Point::new(m.glass_x() + m.row_h as i32, m.row_top(i as usize) + m.row_h as i32 / 2)
}

fn on_mute(i: i32) -> Point {
    let m = m56();
    Point::new(m.mute_zone_left() + m.pad as i32, m.row_top(i as usize) + m.row_h as i32 / 2)
}

#[test]
fn folding_reports_repaints_only_for_visible_change() {
    let mut panel = AudioPanel::new();
    assert!(panel.fold_sinks(parse_sinks(SINKS_JSON)), "the first list is news");
    assert!(!panel.fold_sinks(parse_sinks(SINKS_JSON)), "the same list again is not");
    assert!(panel.fold_default(Some(VOLT.to_string())), "the lamp lights");
    assert!(!panel.fold_default(Some(VOLT.to_string())), "the same lamp is not a repaint");
    assert!(!panel.fold_inputs(parse_sink_inputs(SINK_INPUTS_JSON)), "streams are invisible");
    assert!(panel.fold_sinks(None), "losing the list is a visible death");
    assert!(panel.sinks().is_empty());
}

#[test]
fn press_then_release_on_the_same_row_switches_and_predicts() {
    let mut panel = fixture_panel();
    assert!(panel.on_press(on_row(0), &m56()));
    let (action, repaint) = panel.on_release(on_row(0), &m56());
    assert_eq!(action, Some(PanelAction::SwitchTo(HDMI.to_string())));
    assert!(repaint);
    assert_eq!(panel.shown_default(), Some(HDMI), "the lamp moves before pactl answers — the prediction");
}

#[test]
fn a_slip_off_the_row_cancels_instead_of_switching() {
    let mut panel = fixture_panel();
    panel.on_press(on_row(0), &m56());
    let (action, repaint) = panel.on_release(on_row(1), &m56());
    assert_eq!(action, None, "release elsewhere is a cancel");
    assert!(repaint, "the pressed visual still clears");
    assert_eq!(panel.shown_default(), Some(VOLT), "no prediction from a cancel");

    // Leaving the panel entirely disarms the press too.
    panel.on_press(on_row(0), &m56());
    assert!(panel.on_leave());
    let (action, _) = panel.on_release(on_row(0), &m56());
    assert_eq!(action, None, "a press the pointer abandoned cannot fire");
}

#[test]
fn clicking_the_shown_default_asks_for_nothing() {
    let mut panel = fixture_panel();
    panel.on_press(on_row(1), &m56());
    let (action, _) = panel.on_release(on_row(1), &m56());
    assert_eq!(action, None, "it already is the default");
}

#[test]
fn the_mute_square_is_its_own_control() {
    let mut panel = fixture_panel();
    panel.on_press(on_mute(1), &m56());
    let (action, _) = panel.on_release(on_mute(1), &m56());
    assert_eq!(action, Some(PanelAction::ToggleMute(VOLT.to_string())));
    assert_eq!(panel.shown_default(), Some(VOLT), "muting is not switching");

    // Press on the mute square, release on the row body: different
    // controls, no action — even at the same row.
    panel.on_press(on_mute(0), &m56());
    let (action, _) = panel.on_release(on_row(0), &m56());
    assert_eq!(action, None);
}

#[test]
fn unavailable_rows_are_inert_scenery() {
    let mut panel = fixture_panel();
    assert!(!panel.on_press(on_row(2), &m56()), "row 2's port is physically absent");
    let (action, _) = panel.on_release(on_row(2), &m56());
    assert_eq!(action, None);
    assert!(!panel.on_motion(on_row(2), &m56()), "no hover on scenery");
    assert!(panel.hover().is_none());
}

#[test]
fn hover_tracks_the_control_under_the_pointer() {
    let mut panel = fixture_panel();
    assert!(panel.on_motion(on_row(0), &m56()), "arriving on a row is a repaint");
    assert!(!panel.on_motion(on_row(0), &m56()), "staying put is not");
    assert!(panel.on_motion(on_mute(0), &m56()), "crossing the notch to the mute square is");
    assert_eq!(panel.hover(), Some(&PanelTarget { sink: HDMI.to_string(), zone: PanelZone::Mute }));
    assert!(panel.on_leave());
    assert!(panel.hover().is_none());
}

/// The wheel over a row is that device's knob: one notch, one 5% step,
/// computed absolutely off the sampled level so `pactl` — which has no
/// `-l 1.0` — cannot be walked into overdrive.
#[test]
fn the_wheel_over_a_row_steps_that_devices_level() {
    let panel = fixture_panel();
    // Row 0 is the HDMI at 40%.
    assert_eq!(
        panel.on_scroll(on_row(0), 1, &m56()),
        Some(PanelAction::SetVolume { sink: HDMI.to_string(), percent: 45 })
    );
    assert_eq!(
        panel.on_scroll(on_row(0), -1, &m56()),
        Some(PanelAction::SetVolume { sink: HDMI.to_string(), percent: 35 })
    );
    // A burst report is still one step: the level asked for is absolute.
    assert_eq!(
        panel.on_scroll(on_row(0), 1000, &m56()),
        Some(PanelAction::SetVolume { sink: HDMI.to_string(), percent: 45 })
    );
    assert_eq!(panel.on_scroll(on_row(0), 0, &m56()), None, "no travel asks for nothing");

    // The mute square is part of the same knob, exactly as the tile's
    // whole face is.
    assert_eq!(
        panel.on_scroll(on_mute(0), 1, &m56()),
        Some(PanelAction::SetVolume { sink: HDMI.to_string(), percent: 45 })
    );

    // Row 2 is the unplugged S/PDIF: inert to the wheel like everything
    // else about it, and nowhere is nowhere.
    assert_eq!(panel.on_scroll(on_row(2), 1, &m56()), None);
    assert_eq!(panel.on_scroll(Point::new(40, 0), 1, &m56()), None);
}

#[test]
fn the_wheel_stops_at_the_ceiling_and_the_floor() {
    let mut panel = fixture_panel();
    // Row 1, the Volt, is already at 100%.
    assert_eq!(panel.on_scroll(on_row(1), 1, &m56()), None, "the knob does not climb past full scale");
    assert_eq!(
        panel.on_scroll(on_row(1), -1, &m56()),
        Some(PanelAction::SetVolume { sink: VOLT.to_string(), percent: 95 })
    );

    let mut sinks = fixture_sinks();
    sinks[0].volume_percent = Some(2);
    sinks[1].volume_percent = Some(153);
    sinks[2].volume_percent = None;
    panel.fold_sinks(Some(sinks));
    assert_eq!(
        panel.on_scroll(on_row(0), -1, &m56()),
        Some(PanelAction::SetVolume { sink: HDMI.to_string(), percent: 0 }),
        "the last part-step lands on silence rather than underflowing"
    );
    assert_eq!(
        panel.on_scroll(on_row(1), 1, &m56()),
        None,
        "the wheel will not add overdrive to a sink something else already overdrove"
    );
    assert_eq!(
        panel.on_scroll(on_row(1), -1, &m56()),
        Some(PanelAction::SetVolume { sink: VOLT.to_string(), percent: 148 }),
        "but it steps such a sink back down rather than snapping it to full scale"
    );

    // A sink with no readable level has no knob: there is nothing to
    // step from, and inventing a zero would be a mute by accident.
    let mut sinks = fixture_sinks();
    sinks[0].volume_percent = None;
    panel.fold_sinks(Some(sinks));
    assert_eq!(panel.on_scroll(on_row(0), 1, &m56()), None);
}

#[test]
fn a_press_survives_the_list_reordering_under_it() {
    let mut panel = fixture_panel();
    panel.on_press(on_row(0), &m56());
    // The list reorders: the Volt takes row 0.
    let mut sinks = fixture_sinks();
    sinks.swap(0, 1);
    panel.fold_sinks(Some(sinks));
    let (action, _) = panel.on_release(on_row(0), &m56());
    assert_eq!(action, None, "row 0 is a different device now — the press must not fire on it");
}

#[test]
fn the_prediction_confirms_quietly_and_expires_honestly() {
    let mut panel = fixture_panel();
    panel.on_press(on_row(0), &m56());
    panel.on_release(on_row(0), &m56());
    assert_eq!(panel.shown_default(), Some(HDMI));

    // The resample lands and agrees: the prediction retires with no
    // visible change.
    assert!(!panel.fold_default(Some(HDMI.to_string())), "confirmation moves no pixels");
    assert_eq!(panel.shown_default(), Some(HDMI));

    // And it stays the truth even as later samples repeat it.
    assert!(!panel.fold_default(Some(HDMI.to_string())));
}

#[test]
fn an_unconfirmed_prediction_falls_back_to_the_sampled_truth() {
    let mut panel = fixture_panel();
    panel.on_press(on_row(0), &m56());
    panel.on_release(on_row(0), &m56());
    assert_eq!(panel.shown_default(), Some(HDMI));

    // The mixer keeps answering with the old default: the lamp holds
    // its optimism for PREDICTION_SAMPLES readings, then concedes.
    for i in 1..PREDICTION_SAMPLES {
        assert!(!panel.fold_default(Some(VOLT.to_string())), "reading {i}: still believing the click");
        assert_eq!(panel.shown_default(), Some(HDMI));
    }
    assert!(panel.fold_default(Some(VOLT.to_string())), "the expiry is a visible fall-back");
    assert_eq!(panel.shown_default(), Some(VOLT), "the sample is the truth; the lamp was a prediction");
}

// -----------------------------------------------------------------
// The renderer
// -----------------------------------------------------------------

fn ctx() -> (cosmic_text::FontSystem, cosmic_text::SwashCache) {
    (cosmic_text::FontSystem::new(), cosmic_text::SwashCache::new())
}

/// The metrics a panel of `rows` devices gets when the shell grants the
/// request in full at `tile` — the everyday case.
fn granted(tile: u32, rows: usize) -> PanelMetrics {
    let (w, h) = PanelMetrics::request(tile, rows);
    PanelMetrics::granted(tile, w, h, rows)
}

#[test]
fn the_buffer_is_exactly_the_grant_at_both_scales() {
    let theme = nextstep_classic();
    let (mut fs, mut sc) = ctx();
    let panel = fixture_panel();
    for tile in [56u32, 112] {
        let m = granted(tile, 3);
        let buf = render_audio_panel(&theme, &mut fs, &mut sc, &m, &panel);
        assert_eq!((buf.width, buf.height), (m.width, m.height), "tile {tile}");
        assert_eq!(buf.pixels.len(), (buf.width * buf.height * 4) as usize);
    }
}

/// A clamped grant is filled, not overdrawn: the buffer is the granted
/// size to the pixel and every one of those pixels is painted.
#[test]
fn a_clamped_grant_is_filled_floor_to_floor() {
    let theme = nextstep_classic();
    let (mut fs, mut sc) = ctx();
    let panel = fixture_panel();
    for (w, h) in [(240, 72), (336, 40), (400, 300), (336, 108)] {
        let m = PanelMetrics::granted(56, w, h, 3);
        let buf = render_audio_panel(&theme, &mut fs, &mut sc, &m, &panel);
        assert_eq!((buf.width, buf.height), (w, h), "grant {w}x{h}");
        assert!(buf.pixels.chunks_exact(4).all(|px| px[3] == 255), "grant {w}x{h}: no hole under a clamped panel");
    }
}

#[test]
fn rendering_is_a_pure_function_of_state() {
    let theme = nextstep_classic();
    let (mut fs, mut sc) = ctx();
    let panel = fixture_panel();
    let a = render_audio_panel(&theme, &mut fs, &mut sc, &m56(), &panel);
    let b = render_audio_panel(&theme, &mut fs, &mut sc, &m56(), &panel);
    assert_eq!(a.pixels, b.pixels, "same state, same bytes");
}

#[test]
fn every_interactive_state_renders_distinctly() {
    let theme = nextstep_classic();
    let (mut fs, mut sc) = ctx();
    let mut panel = fixture_panel();
    let idle = render_audio_panel(&theme, &mut fs, &mut sc, &m56(), &panel).pixels;

    panel.on_motion(on_row(0), &m56());
    let hovered = render_audio_panel(&theme, &mut fs, &mut sc, &m56(), &panel).pixels;
    assert_ne!(idle, hovered, "hover must show");

    panel.on_press(on_row(0), &m56());
    let pressed = render_audio_panel(&theme, &mut fs, &mut sc, &m56(), &panel).pixels;
    assert_ne!(hovered, pressed, "the armed row sinks");

    panel.on_motion(on_mute(0), &m56());
    panel.on_press(on_mute(0), &m56());
    let mute_pressed = render_audio_panel(&theme, &mut fs, &mut sc, &m56(), &panel).pixels;
    assert_ne!(pressed, mute_pressed, "the mute square arms on its own");
}

#[test]
fn the_default_lamp_follows_the_shown_default() {
    let theme = nextstep_classic();
    let (mut fs, mut sc) = ctx();
    let mut panel = fixture_panel();
    let volt_default = render_audio_panel(&theme, &mut fs, &mut sc, &m56(), &panel).pixels;
    panel.fold_default(Some(HDMI.to_string()));
    let hdmi_default = render_audio_panel(&theme, &mut fs, &mut sc, &m56(), &panel).pixels;
    assert_ne!(volt_default, hdmi_default, "the lamp must move with the default");
}

#[test]
fn muted_and_running_and_unavailable_all_read_on_the_face() {
    let theme = nextstep_classic();
    let (mut fs, mut sc) = ctx();
    let mut panel = AudioPanel::new();
    let base: Vec<AudioSink> = fixture_sinks()
        .into_iter()
        .map(|s| AudioSink { muted: false, running: false, available: true, ..s })
        .collect();
    panel.fold_sinks(Some(base.clone()));
    let plain = render_audio_panel(&theme, &mut fs, &mut sc, &m56(), &panel).pixels;

    let mut muted = base.clone();
    muted[1].muted = true;
    panel.fold_sinks(Some(muted));
    let muted_px = render_audio_panel(&theme, &mut fs, &mut sc, &m56(), &panel).pixels;
    assert_ne!(plain, muted_px, "a muted sink must wear the strike");

    let mut running = base.clone();
    running[1].running = true;
    panel.fold_sinks(Some(running));
    let running_px = render_audio_panel(&theme, &mut fs, &mut sc, &m56(), &panel).pixels;
    assert_ne!(plain, running_px, "a playing sink must show its bars");

    let mut unavailable = base.clone();
    unavailable[1].available = false;
    panel.fold_sinks(Some(unavailable));
    let unavailable_px = render_audio_panel(&theme, &mut fs, &mut sc, &m56(), &panel).pixels;
    assert_ne!(plain, unavailable_px, "an unplugged sink must grey out");
}

#[test]
fn the_empty_panel_is_a_face_not_a_hole() {
    let (mut fs, mut sc) = ctx();
    for theme in all_themes() {
        let panel = AudioPanel::new();
        let m = granted(56, 0);
        let buf = render_audio_panel(&theme, &mut fs, &mut sc, &m, &panel);
        assert_eq!((buf.width, buf.height), (m.width, m.height), "theme {}", theme.id);
        assert!(buf.pixels.chunks_exact(4).all(|px| px[3] == 255), "theme {}: fully painted, fully opaque", theme.id);
    }
}

#[test]
fn every_theme_renders_the_populated_panel_opaquely() {
    let (mut fs, mut sc) = ctx();
    let panel = fixture_panel();
    for theme in all_themes() {
        let buf = render_audio_panel(&theme, &mut fs, &mut sc, &m56(), &panel);
        assert!(buf.pixels.chunks_exact(4).all(|px| px[3] == 255), "theme {}: the panel ground must be opaque", theme.id);
    }
}
