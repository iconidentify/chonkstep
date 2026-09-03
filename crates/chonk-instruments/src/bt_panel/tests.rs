//! The panel's behavior, against canned BlueZ replies.
//!
//! Every fixture here is hand-written, and that is a statement about
//! this machine rather than a shortcut: it has no Bluetooth controller
//! (`/sys/class/bluetooth` does not exist, `bluetooth.service` is
//! inactive, `busctl` cannot activate `org.bluez`), so **no test in
//! this file has ever run against a real adapter, and none of them can
//! here.** What they do pin is everything on this side of the sampler
//! boundary — the fold, the row grammar, the hit-test, the confirm
//! machine and the exact argv of every action — which is the part the
//! `Source`/`Effect` split exists to make testable without hardware.
//! The shape of the replies is `busctl --json=short`'s, captured live
//! from a service this machine *does* run; see [`super::bluez`].

use std::time::{Duration, Instant};

use chonk_dock_widget::{Effect, PanelEvent, PanelReaction, SourceId};
use wm_theme::bluetooth::{forget_cell_width, panel_content_width, panel_row_height, BtPanelRow};
use wm_theme_api::Point;

use super::bluez::{parse_managed_objects, BluezState, RfkillState};
use super::{BtPanel, FORGET_GRACE, PENDING_DEADLINE_SAMPLES};

const TILE: u32 = 56;

fn width() -> u32 {
    panel_content_width(TILE)
}

fn row_h() -> u32 {
    panel_row_height(TILE)
}

/// A reply with one adapter and the devices named, each
/// `(name, connected, paired)`.
fn state(powered: bool, devices: &[(&str, bool, bool)]) -> BluezState {
    let mut objects =
        vec![format!(r#""/org/bluez/hci0":{{"org.bluez.Adapter1":{{"Powered":{{"type":"b","data":{powered}}}}}}}"#)];
    for (index, (name, connected, paired)) in devices.iter().enumerate() {
        objects.push(format!(
            r#""/org/bluez/hci0/dev_00_00_00_00_00_{index:02X}":{{"org.bluez.Device1":{{"Alias":{{"type":"s","data":"{name}"}},"Connected":{{"type":"b","data":{connected}}},"Paired":{{"type":"b","data":{paired}}}}}}}"#
        ));
    }
    parse_managed_objects(&format!(r#"{{"type":"a{{oa{{sa{{sv}}}}}}","data":[{{{}}}]}}"#, objects.join(",")))
        .expect("fixture parses")
}

fn device_path(index: usize) -> String {
    format!("/org/bluez/hci0/dev_00_00_00_00_00_{index:02X}")
}

fn clear() -> RfkillState {
    RfkillState { present: true, soft: false, hard: false }
}

/// A panel already fed one sample, bound to source 0.
fn fixture(powered: bool, devices: &[(&str, bool, bool)], rfkill: RfkillState) -> (BtPanel, BluezState) {
    let mut panel = BtPanel::new();
    panel.bind(SourceId::from_index(0));
    let bluez = state(powered, devices);
    panel.set_state(true, &bluez, rfkill, true);
    (panel, bluez)
}

/// A click at the vertical middle of row `index`, `from_right` pixels
/// in from the panel's right edge.
fn click_row(index: usize, from_right: u32) -> PanelEvent {
    PanelEvent::LeftPress {
        local: Point::new((width() - from_right) as i32, (index as u32 * row_h() + row_h() / 2) as i32),
    }
}

/// A click on a row's body — well clear of the forget cell.
fn click_body(index: usize) -> PanelEvent {
    click_row(index, forget_cell_width(row_h()) + row_h())
}

/// A click on a row's `[x]`.
fn click_forget(index: usize) -> PanelEvent {
    click_row(index, forget_cell_width(row_h()) / 2)
}

fn run_args(reaction: &PanelReaction) -> (&'static str, Vec<String>) {
    match reaction {
        PanelReaction::Run(Effect::Run { program, args, .. }) => (program, args.clone()),
        _ => panic!("expected a Run reaction"),
    }
}

// -- the row grammar --------------------------------------------------

#[test]
fn the_rows_are_power_then_connected_then_known_then_pair_new() {
    let (panel, _) = fixture(true, &[("MX Keys", false, true), ("WH-1000XM4", true, true), ("Ghost", false, false)], clear());
    let rows = panel.render_rows();
    assert!(matches!(rows[0], BtPanelRow::Power { on: true }));
    // Connected first, whatever order BlueZ listed them in.
    assert!(matches!(rows[1], BtPanelRow::Device { name: "WH-1000XM4", connected: true, .. }));
    assert!(matches!(rows[2], BtPanelRow::Device { name: "MX Keys", connected: false, .. }));
    assert!(matches!(rows[3], BtPanelRow::PairNew));
    assert_eq!(rows.len(), 4, "an unpaired stranger is not a known device and does not get a row");
}

#[test]
fn a_machine_with_no_adapter_shows_exactly_one_row() {
    let mut panel = BtPanel::new();
    panel.set_state(false, &BluezState::default(), RfkillState::default(), true);
    let rows = panel.render_rows();
    assert_eq!(rows.len(), 1);
    assert!(matches!(rows[0], BtPanelRow::NoAdapter));
    // And every click on it is inert — there is nothing here to do.
    assert!(matches!(panel.input(click_body(0), TILE, width()), PanelReaction::None));
}

#[test]
fn the_spec_grows_with_the_rows() {
    let (empty, _) = fixture(true, &[], clear());
    let (full, _) = fixture(true, &[("A", true, true), ("B", false, true)], clear());
    assert_eq!(empty.spec(TILE).width, full.spec(TILE).width, "width is a constant of the dock, not of the content");
    assert!(full.spec(TILE).height > empty.spec(TILE).height, "two more devices is two more rows");
    assert_eq!(full.spec(TILE).height - empty.spec(TILE).height, row_h() * 2);
}

// -- the power row ----------------------------------------------------

#[test]
fn the_power_row_follows_the_rfkill_order() {
    // Soft-blocked: unblock, and never a power-on into a block.
    let (mut blocked, _) = fixture(false, &[], RfkillState { present: true, soft: true, hard: false });
    let (program, args) = run_args(&blocked.input(click_body(0), TILE, width()));
    assert_eq!((program, args), ("rfkill", vec!["unblock".to_string(), "bluetooth".to_string()]));

    // On: block, because that is the half that survives a reboot.
    let (mut on, _) = fixture(true, &[], clear());
    let (program, args) = run_args(&on.input(click_body(0), TILE, width()));
    assert_eq!((program, args), ("rfkill", vec!["block".to_string(), "bluetooth".to_string()]));

    // Unblocked but dark: ask BlueZ directly.
    let (mut dark, _) = fixture(false, &[], clear());
    let (program, args) = run_args(&dark.input(click_body(0), TILE, width()));
    assert_eq!(program, "busctl");
    assert_eq!(args.first().map(String::as_str), Some("--system"));
    assert!(args.contains(&"/org/bluez/hci0".to_string()) && args.last() == Some(&"true".to_string()));
}

#[test]
fn a_hard_block_offers_nothing_because_nothing_would_work() {
    let (mut panel, _) = fixture(false, &[], RfkillState { present: true, soft: true, hard: true });
    assert!(matches!(panel.input(click_body(0), TILE, width()), PanelReaction::None));
}

// -- connect and disconnect -------------------------------------------

#[test]
fn clicking_a_known_device_connects_it_and_a_connected_one_disconnects() {
    let (mut panel, _) = fixture(true, &[("WH-1000XM4", true, true), ("MX Keys", false, true)], clear());

    let (program, args) = run_args(&panel.input(click_body(1), TILE, width()));
    assert_eq!(program, "busctl");
    assert_eq!(args, vec!["--system", "call", "org.bluez", &device_path(0), "org.bluez.Device1", "Disconnect"]);

    let (program, args) = run_args(&panel.input(click_body(2), TILE, width()));
    assert_eq!(program, "busctl");
    assert_eq!(args, vec!["--system", "call", "org.bluez", &device_path(1), "org.bluez.Device1", "Connect"]);
}

/// The runtime argument is BlueZ's own object path, carried as one
/// argv element. Nothing here is ever interpreted by a shell.
#[test]
fn the_only_runtime_argument_is_an_object_path_in_one_argv_element() {
    let (mut panel, _) = fixture(true, &[("Weird; rm -rf /", false, true)], clear());
    let (_, args) = run_args(&panel.input(click_body(1), TILE, width()));
    assert!(args.iter().all(|arg| !arg.contains(' ') || arg.starts_with("/org/bluez")), "no argv element carries loose text: {args:?}");
    assert!(args.contains(&device_path(0)), "the device is named by path, never by its name");
}

#[test]
fn a_clicked_device_reads_as_pending_until_a_sample_agrees() {
    let (mut panel, _) = fixture(true, &[("MX Keys", false, true)], clear());
    panel.input(click_body(1), TILE, width());
    assert!(matches!(panel.render_rows()[1], BtPanelRow::Device { pending: true, .. }), "the request is visible at once");

    // A fresh sample that still disagrees keeps it pending...
    panel.set_state(true, &state(true, &[("MX Keys", false, true)]), clear(), true);
    assert!(matches!(panel.render_rows()[1], BtPanelRow::Device { pending: true, .. }));

    // ...and the sample that agrees settles it.
    panel.set_state(true, &state(true, &[("MX Keys", true, true)]), clear(), true);
    let rows = panel.render_rows();
    assert!(matches!(rows[1], BtPanelRow::Device { connected: true, pending: false, .. }));
}

/// An instrument that keeps saying "connecting…" after the system gave
/// up is lying with extra steps.
#[test]
fn a_pending_that_outlives_its_deadline_goes_back_to_showing_reality() {
    let (mut panel, _) = fixture(true, &[("MX Keys", false, true)], clear());
    panel.input(click_body(1), TILE, width());
    for _ in 0..PENDING_DEADLINE_SAMPLES {
        assert!(matches!(panel.render_rows()[1], BtPanelRow::Device { pending: true, .. }));
        panel.set_state(true, &state(true, &[("MX Keys", false, true)]), clear(), true);
    }
    assert!(matches!(panel.render_rows()[1], BtPanelRow::Device { pending: false, connected: false, .. }));
}

/// Stale passes must not spend a deadline — the budget counts
/// *readings*, not repaints, and the dock ticks a panel at ~60Hz.
#[test]
fn a_stale_pass_spends_no_deadline() {
    let (mut panel, _) = fixture(true, &[("MX Keys", false, true)], clear());
    panel.input(click_body(1), TILE, width());
    for _ in 0..100 {
        panel.set_state(true, &state(true, &[("MX Keys", false, true)]), clear(), false);
    }
    assert!(matches!(panel.render_rows()[1], BtPanelRow::Device { pending: true, .. }), "sixty passes a second must not expire a request");
}

#[test]
fn clicking_a_pending_device_again_does_nothing() {
    let (mut panel, _) = fixture(true, &[("MX Keys", false, true)], clear());
    panel.input(click_body(1), TILE, width());
    assert!(
        matches!(panel.input(click_body(1), TILE, width()), PanelReaction::None),
        "a second Connect mid-negotiation is how a pairing gets confused"
    );
}

/// A device that disappears from the reply — forgotten, or the adapter
/// went down — has no pending state left to show.
#[test]
fn a_vanished_device_settles_its_pending() {
    let (mut panel, _) = fixture(true, &[("MX Keys", false, true)], clear());
    panel.input(click_body(1), TILE, width());
    panel.set_state(true, &state(true, &[]), clear(), true);
    assert_eq!(panel.render_rows().len(), 2, "power and pair-new remain");
}

// -- the two-click forget ---------------------------------------------

#[test]
fn forgetting_takes_two_clicks_on_the_same_cell() {
    let (mut panel, _) = fixture(true, &[("MX Keys", false, true)], clear());
    let now = Instant::now();
    panel.tick(now);

    // First click arms and only arms: no command leaves the panel.
    assert!(matches!(panel.input(click_forget(1), TILE, width()), PanelReaction::Repaint));
    assert!(matches!(panel.render_rows()[1], BtPanelRow::Device { armed: true, .. }), "the pending question is on the face");

    // Second click commits, and names the adapter's RemoveDevice.
    let (program, args) = run_args(&panel.input(click_forget(1), TILE, width()));
    assert_eq!(program, "busctl");
    assert_eq!(
        args,
        vec!["--system", "call", "org.bluez", "/org/bluez/hci0", "org.bluez.Adapter1", "RemoveDevice", "o", &device_path(0)]
    );
}

#[test]
fn an_arming_expires_on_its_own() {
    let (mut panel, _) = fixture(true, &[("MX Keys", false, true)], clear());
    let now = Instant::now();
    panel.tick(now);
    panel.input(click_forget(1), TILE, width());
    assert!(matches!(panel.render_rows()[1], BtPanelRow::Device { armed: true, .. }));

    // A tick past the grace window disarms it, and says so.
    assert!(panel.tick(now + FORGET_GRACE + Duration::from_millis(1)), "the cell going back to a whisper is a repaint");
    assert!(matches!(panel.render_rows()[1], BtPanelRow::Device { armed: false, .. }));

    // And a click that lands after the expiry only re-arms.
    let reaction = panel.input(click_forget(1), TILE, width());
    assert!(matches!(reaction, PanelReaction::Repaint), "an expired confirm must not forget the device");
}

#[test]
fn a_second_click_after_the_grace_window_re_arms_rather_than_forgetting() {
    let (mut panel, _) = fixture(true, &[("MX Keys", false, true)], clear());
    let now = Instant::now();
    panel.tick(now);
    panel.input(click_forget(1), TILE, width());
    // Time passes, but no tick lands to expire it; the click itself
    // must still notice the window has closed.
    panel.tick(now + FORGET_GRACE + Duration::from_secs(1));
    assert!(matches!(panel.input(click_forget(1), TILE, width()), PanelReaction::Repaint));
}

#[test]
fn anything_else_disarms_the_confirm() {
    let now = Instant::now();

    // A click on a different row's body.
    let (mut panel, _) = fixture(true, &[("MX Keys", false, true)], clear());
    panel.tick(now);
    panel.input(click_forget(1), TILE, width());
    panel.input(click_body(0), TILE, width());
    assert!(matches!(panel.render_rows()[1], BtPanelRow::Device { armed: false, .. }));

    // A click outside every row.
    let (mut panel, _) = fixture(true, &[("MX Keys", false, true)], clear());
    panel.tick(now);
    panel.input(click_forget(1), TILE, width());
    let below = PanelEvent::LeftPress { local: Point::new(4, (row_h() * 40) as i32) };
    assert!(matches!(panel.input(below, TILE, width()), PanelReaction::Repaint));
    assert!(matches!(panel.render_rows()[1], BtPanelRow::Device { armed: false, .. }));
}

#[test]
fn arming_one_cell_disarms_another() {
    let (mut panel, _) = fixture(true, &[("A", false, true), ("B", false, true)], clear());
    panel.tick(Instant::now());
    panel.input(click_forget(1), TILE, width());
    panel.input(click_forget(2), TILE, width());
    let rows = panel.render_rows();
    assert!(matches!(rows[1], BtPanelRow::Device { armed: false, .. }), "only one question at a time");
    assert!(matches!(rows[2], BtPanelRow::Device { armed: true, .. }));
}

/// The body of a device row and its `[x]` are different actions, and
/// the boundary is the one `wm-theme` draws at.
#[test]
fn the_forget_cell_and_the_row_body_are_different_clicks() {
    let (mut panel, _) = fixture(true, &[("MX Keys", false, true)], clear());
    panel.tick(Instant::now());
    // One pixel inside the cell arms; one pixel outside it connects.
    let cell = forget_cell_width(row_h());
    assert!(matches!(panel.input(click_row(1, cell), TILE, width()), PanelReaction::Repaint), "the cell's first column arms");
    let reaction = panel.input(click_row(1, cell + 1), TILE, width());
    assert_eq!(run_args(&reaction).0, "busctl", "one pixel to the left is the row body");
}

// -- the pairing dialog ------------------------------------------------

#[test]
fn the_pair_new_row_spawns_the_dialog_and_asks_for_no_resample() {
    let (mut panel, _) = fixture(true, &[], clear());
    match panel.input(click_body(1), TILE, width()) {
        PanelReaction::Run(Effect::Run { program, args, then }) => {
            assert_eq!(program, super::PAIR_DIALOG);
            assert!(args.is_empty(), "the dialog discovers its own adapter");
            assert!(then.is_none(), "the sample that matters lands long after the dialog exits");
        }
        _ => panic!("the pair-new row must spawn the dialog"),
    }
}

// -- repaint bookkeeping ----------------------------------------------

/// An open panel costs one boolean per pass and one repaint per actual
/// change — the shape `panel_tick`'s doc prescribes.
#[test]
fn an_idle_panel_asks_for_no_repaints() {
    let (mut panel, bluez) = fixture(true, &[("MX Keys", false, true)], clear());
    panel.render_rows();
    panel.tick(Instant::now());
    // Drain the construction-time dirty flag, then hold still.
    let mut fonts = cosmic_text::FontSystem::new();
    let mut swash = cosmic_text::SwashCache::new();
    let theme = wm_theme::default_theme::nextstep_classic();
    panel.render(&theme, TILE, width(), row_h() * 3, &mut fonts, &mut swash);

    let now = Instant::now();
    for step in 0..10 {
        panel.set_state(true, &bluez, clear(), true);
        assert!(!panel.tick(now + Duration::from_millis(step * 16)), "an unchanged panel must not repaint");
    }
}

#[test]
fn a_changed_device_list_asks_for_a_repaint() {
    let (mut panel, _) = fixture(true, &[], clear());
    let mut fonts = cosmic_text::FontSystem::new();
    let mut swash = cosmic_text::SwashCache::new();
    let theme = wm_theme::default_theme::nextstep_classic();
    panel.render(&theme, TILE, width(), row_h() * 2, &mut fonts, &mut swash);
    let now = Instant::now();
    assert!(!panel.tick(now));

    panel.set_state(true, &state(true, &[("Buds", true, true)]), clear(), true);
    assert!(panel.tick(now), "a device arriving is news");
}

/// The panel renders at the *granted* size, which on a crowded screen
/// is smaller than the one it asked for.
#[test]
fn the_panel_renders_into_a_clamped_grant() {
    let (mut panel, _) = fixture(true, &[("A", true, true), ("B", false, true)], clear());
    let mut fonts = cosmic_text::FontSystem::new();
    let mut swash = cosmic_text::SwashCache::new();
    let theme = wm_theme::default_theme::nextstep_classic();
    let asked = panel.spec(TILE);
    let granted_h = asked.height - row_h();
    let buffer = panel.render(&theme, TILE, asked.width, granted_h, &mut fonts, &mut swash);
    assert_eq!((buffer.width, buffer.height), (asked.width, granted_h), "the grant wins, and the rows clip rather than squeeze");
}
