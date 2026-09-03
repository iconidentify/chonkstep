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
//!
//! The one state this desk *can* reach is [`BtStatus::NoRadio`], and
//! it is the state the panel's whole redesign is about, so it is
//! pinned harder than the rest: that it offers no control at all, that
//! it is a plate rather than a sliver, and that it does not look like
//! either of the other two ways to have no Bluetooth.

use std::time::{Duration, Instant};

use chonk_dock_widget::{Effect, PanelEvent, PanelReaction, SourceId};
use wm_theme_api::Point;

use super::bluez::{parse_managed_objects, BluezState, RfkillState};
use super::render::{self, Block, BtLayout, BtRowKey, BtStatus, DeviceClass};
use super::{BtPanel, FORGET_GRACE, PENDING_DEADLINE_SAMPLES};

const TILE: u32 = 56;

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

/// A panel already fed one sample from a real controller, bound to
/// source 0.
fn fixture(powered: bool, devices: &[(&str, bool, bool)], rfkill: RfkillState) -> (BtPanel, BluezState) {
    let mut panel = BtPanel::new();
    panel.bind(SourceId::from_index(0));
    let bluez = state(powered, devices);
    panel.set_state(Some("hci0"), &bluez, rfkill, true);
    (panel, bluez)
}

fn layout(panel: &BtPanel) -> BtLayout {
    render::bt_layout(&panel.view(), TILE)
}

/// The middle of a control, in panel-local pixels — the layout's own
/// answer to where it drew the thing, so a test can never click
/// somewhere the renderer did not put a cell.
fn center(panel: &BtPanel, key: &BtRowKey) -> Point {
    let (x, y, w, h) = layout(panel).row_rect(key).unwrap_or_else(|| panic!("no band for {key:?}"));
    Point::new(x + w as i32 / 2, y + h as i32 / 2)
}

/// One full click: press and release on the same control, which is the
/// only gesture that fires anything.
fn click(panel: &mut BtPanel, key: &BtRowKey) -> PanelReaction {
    let at = center(panel, key);
    panel.on_event(PanelEvent::LeftPress { local: at }, TILE);
    panel.on_event(PanelEvent::LeftRelease { local: at }, TILE)
}

fn device_key(index: usize) -> BtRowKey {
    BtRowKey::Device(device_path(index))
}

fn forget_key(index: usize) -> BtRowKey {
    BtRowKey::Forget(device_path(index))
}

fn run_args(reaction: &PanelReaction) -> (&'static str, Vec<String>) {
    match reaction {
        PanelReaction::Run(Effect::Run { program, args, .. }) => (program, args.clone()),
        _ => panic!("expected a Run reaction"),
    }
}

// -- the three absences -----------------------------------------------

/// The state this machine is actually in. It is a designed plate, not
/// an error slug: nothing on it is clickable, because there is nothing
/// here to click.
#[test]
fn a_machine_with_no_radio_is_a_plate_with_no_controls_on_it() {
    let mut panel = BtPanel::new();
    panel.bind(SourceId::from_index(0));
    panel.set_state(None, &BluezState::default(), RfkillState::default(), true);

    let view = panel.view();
    assert_eq!(view.status, BtStatus::NoRadio);
    assert!(view.devices.is_empty(), "no adapter answered, so there is nothing it knows");

    let layout = layout(&panel);
    assert_eq!(layout.row_rect(&BtRowKey::Power), None, "there is no radio to offer to power");
    assert_eq!(layout.row_rect(&BtRowKey::PairNew), None, "and nothing to pair it with");
    // Every point in the panel, on a coarse grid: none of it acts.
    for y in (0..layout.height as i32).step_by(4) {
        for x in (0..layout.width as i32).step_by(8) {
            assert_eq!(layout.row_at(x, y), None, "({x},{y}) must not be a control");
        }
    }
}

/// The defect this redesign exists to fix: the no-radio panel used to
/// be a ~600x50 sliver with the words `NO ADAPTER` centred in it,
/// which read as a stub or a rendering bug. It is now a plate — as
/// wide as its sibling LNK panel and tall enough to be a statement.
#[test]
fn the_no_radio_plate_is_panel_sized_not_a_sliver() {
    let mut panel = BtPanel::new();
    panel.bind(SourceId::from_index(0));
    panel.set_state(None, &BluezState::default(), RfkillState::default(), true);

    let spec = panel.spec(112);
    assert_eq!(spec.width, render::panel_width(112), "the width is the family's four tiles");
    assert!(
        spec.height >= render::row_h(112) * 8,
        "the plate needs room for a mark, a headline and its notes; got {}",
        spec.height
    );
    // The old sliver's giveaway was an aspect ratio no designed panel
    // has. Anything near it is the bug coming back.
    assert!(spec.height * 3 > spec.width, "a panel this wide and this short is the sliver again");
}

/// Three ways to have no Bluetooth, three different faces. Folding
/// them together is what made the old panel say `NO ADAPTER` at a
/// machine whose adapter was merely switched off.
#[test]
fn the_three_absences_are_three_different_faces() {
    let mut panel = BtPanel::new();
    panel.bind(SourceId::from_index(0));

    // No hardware: no controls at all.
    panel.set_state(None, &BluezState::default(), RfkillState::default(), true);
    assert_eq!(panel.view().status, BtStatus::NoRadio);
    let no_radio = panel.spec(TILE);
    assert!(layout(&panel).row_rect(&BtRowKey::Power).is_none());

    // Hardware, no daemon: still no power row — BlueZ is not there to
    // be asked — but the panel names the controller and carries the
    // command that would fix it.
    panel.set_state(Some("hci0"), &BluezState::default(), clear(), true);
    assert_eq!(panel.view().status, BtStatus::NoDaemon);
    assert_eq!(panel.view().controller.as_deref(), Some("hci0"));
    let no_daemon = panel.spec(TILE);
    assert!(layout(&panel).row_rect(&BtRowKey::Power).is_none(), "there is no adapter object to power");

    // Hardware and daemon, radio down: a real control, and a real
    // offer behind it.
    panel.set_state(Some("hci0"), &state(false, &[]), clear(), true);
    assert_eq!(panel.view().status, BtStatus::Off { block: Block::None });
    assert!(layout(&panel).row_rect(&BtRowKey::Power).is_some(), "this one is a button");

    assert_ne!(no_radio.height, no_daemon.height, "two absences that lay out identically read identically");
}

/// The block is part of the truth, not a footnote: a soft block
/// survives a reboot and a hard one cannot be cleared from here at
/// all, so the face says which.
#[test]
fn a_block_is_part_of_the_off_face() {
    let (panel, _) = fixture(false, &[], RfkillState { present: true, soft: true, hard: false });
    assert_eq!(panel.view().status, BtStatus::Off { block: Block::Soft });

    let (panel, _) = fixture(false, &[], RfkillState { present: true, soft: true, hard: true });
    assert_eq!(panel.view().status, BtStatus::Off { block: Block::Hard });
}

// -- the row grammar --------------------------------------------------

#[test]
fn the_devices_are_connected_first_then_merely_known() {
    let (panel, _) =
        fixture(true, &[("MX Keys", false, true), ("WH-1000XM4", true, true), ("Ghost", false, false)], clear());
    let view = panel.view();
    assert_eq!(view.status, BtStatus::On { connected: 1 });
    let names: Vec<&str> = view.devices.iter().map(|device| device.name.as_str()).collect();
    assert_eq!(names, ["WH-1000XM4", "MX Keys"], "connected first, then paired-idle; the unpaired ghost is neither");

    let layout = layout(&panel);
    assert!(layout.row_rect(&BtRowKey::Power).is_some());
    assert!(layout.row_rect(&BtRowKey::PairNew).is_some());
    let power_y = layout.row_rect(&BtRowKey::Power).unwrap().1;
    let first_device = layout.row_rect(&device_key(1)).unwrap().1;
    let pair_y = layout.row_rect(&BtRowKey::PairNew).unwrap().1;
    assert!(power_y < first_device && first_device < pair_y, "power, then the devices, then the way to add one");
}

#[test]
fn the_spec_grows_with_the_devices() {
    let (empty, _) = fixture(true, &[], clear());
    let (three, _) = fixture(true, &[("A", true, true), ("B", false, true), ("C", false, true)], clear());
    assert!(three.spec(TILE).height > empty.spec(TILE).height);
    assert_eq!(three.spec(TILE).width, empty.spec(TILE).width, "the width is the family's, not the content's");
}

/// A drawer full of paired junk shows what fits and says how much it
/// did not — a dock panel cannot scroll.
#[test]
fn a_long_device_list_is_capped_and_says_so() {
    let many: Vec<(&str, bool, bool)> = (0..render::MAX_DEVICE_ROWS + 3).map(|_| ("Thing", false, true)).collect();
    let (panel, _) = fixture(true, &many, clear());
    assert_eq!(panel.view().devices.len(), many.len(), "the view carries them all");
    let layout = layout(&panel);
    assert!(layout.row_rect(&device_key(render::MAX_DEVICE_ROWS - 1)).is_some());
    assert!(layout.row_rect(&device_key(render::MAX_DEVICE_ROWS)).is_none(), "past the cap is drawn nowhere");
}

// -- the power row ----------------------------------------------------

/// `omarchy-bluetooth-power`'s order, and its reasons: a soft block is
/// cleared before anything is asked of BlueZ, because a power-on fails
/// outright while the block is set.
#[test]
fn the_power_row_follows_the_rfkill_order() {
    let (mut panel, _) = fixture(false, &[], RfkillState { present: true, soft: true, hard: false });
    let (program, args) = run_args(&click(&mut panel, &BtRowKey::Power));
    assert_eq!((program, args.as_slice()), ("rfkill", &["unblock".to_string(), "bluetooth".to_string()][..]));

    let (mut panel, _) = fixture(false, &[], clear());
    let (program, args) = run_args(&click(&mut panel, &BtRowKey::Power));
    assert_eq!(program, "busctl");
    assert_eq!(args.last().map(String::as_str), Some("true"));
    assert!(args.contains(&"/org/bluez/hci0".to_string()), "the sampled adapter path is the runtime argument");

    // Off is the block, not a `Powered` write: BlueZ never persists
    // `Powered`, and the block is the half that survives a reboot.
    let (mut panel, _) = fixture(true, &[], clear());
    let (program, args) = run_args(&click(&mut panel, &BtRowKey::Power));
    assert_eq!((program, args.as_slice()), ("rfkill", &["block".to_string(), "bluetooth".to_string()][..]));
}

/// A physical kill switch is not ours to flip. The row stays — it is
/// still the truth about the adapter — but it performs nothing.
#[test]
fn a_hard_block_offers_nothing_because_nothing_would_work() {
    let (mut panel, _) = fixture(false, &[], RfkillState { present: true, soft: true, hard: true });
    assert!(matches!(click(&mut panel, &BtRowKey::Power), PanelReaction::Repaint), "the press highlight still clears");
}

// -- devices ----------------------------------------------------------

#[test]
fn clicking_a_known_device_connects_it_and_a_connected_one_disconnects() {
    let (mut panel, _) = fixture(true, &[("WH-1000XM4", true, true), ("MX Keys", false, true)], clear());
    let (program, args) = run_args(&click(&mut panel, &device_key(0)));
    assert_eq!(program, "busctl");
    assert_eq!(args.last().map(String::as_str), Some("Disconnect"));

    let (program, args) = run_args(&click(&mut panel, &device_key(1)));
    assert_eq!(program, "busctl");
    assert_eq!(args.last().map(String::as_str), Some("Connect"));
}

/// The runtime argument is BlueZ's own object path, carried as one
/// argv element. Nothing here is ever interpreted by a shell.
#[test]
fn the_only_runtime_argument_is_an_object_path_in_one_argv_element() {
    let (mut panel, _) = fixture(true, &[("MX Keys", false, true)], clear());
    let (_, args) = run_args(&click(&mut panel, &device_key(0)));
    assert!(args.contains(&device_path(0)), "the path rides whole: {args:?}");
    assert!(args.iter().all(|arg| !arg.contains(' ')), "no argv element is a sentence: {args:?}");
}

/// Device rows are scenery while the radio is down: connecting needs a
/// powered adapter, and a row that looks live and does nothing is
/// worse than one that says it is asleep.
#[test]
fn device_rows_are_inert_while_the_radio_is_off() {
    let (mut panel, _) = fixture(false, &[("MX Keys", false, true)], clear());
    let view = panel.view();
    assert!(!view.devices_live());
    assert_eq!(view.devices.len(), 1, "they are still listed — the panel knows about them");

    let layout = layout(&panel);
    let (x, y, w, h) = layout.row_rect(&device_key(0)).expect("the band exists");
    assert_eq!(layout.row_at(x + w as i32 / 2, y + h as i32 / 2), None, "but the pointer is not offered it");
    assert_eq!(layout.row_rect(&forget_key(0)), None, "and there is no forget key to hit either");

    let at = Point::new(x + w as i32 / 2, y + h as i32 / 2);
    panel.on_event(PanelEvent::LeftPress { local: at }, TILE);
    assert!(matches!(panel.on_event(PanelEvent::LeftRelease { local: at }, TILE), PanelReaction::None));
}

// -- pending ----------------------------------------------------------

#[test]
fn a_clicked_device_reads_as_pending_until_a_sample_agrees() {
    let (mut panel, _) = fixture(true, &[("MX Keys", false, true)], clear());
    click(&mut panel, &device_key(0));
    assert!(panel.view().devices[0].pending, "the row says it asked");

    // A sample that agrees settles it.
    panel.set_state(Some("hci0"), &state(true, &[("MX Keys", true, true)]), clear(), true);
    assert!(!panel.view().devices[0].pending);
    assert!(panel.view().devices[0].connected);
}

/// An instrument that keeps saying "connecting…" after the system gave
/// up is lying with extra steps.
#[test]
fn a_pending_that_outlives_its_deadline_goes_back_to_showing_reality() {
    let (mut panel, bluez) = fixture(true, &[("MX Keys", false, true)], clear());
    click(&mut panel, &device_key(0));
    for _ in 0..PENDING_DEADLINE_SAMPLES {
        assert!(panel.view().devices[0].pending);
        panel.set_state(Some("hci0"), &bluez, clear(), true);
    }
    assert!(!panel.view().devices[0].pending, "the deadline expired and the row shows reality");
}

/// Stale passes must not spend a deadline — the budget counts
/// *readings*, not repaints, and the dock ticks a panel at ~60Hz.
#[test]
fn a_stale_pass_spends_no_deadline() {
    let (mut panel, bluez) = fixture(true, &[("MX Keys", false, true)], clear());
    click(&mut panel, &device_key(0));
    for _ in 0..PENDING_DEADLINE_SAMPLES * 4 {
        panel.set_state(Some("hci0"), &bluez, clear(), false);
    }
    assert!(panel.view().devices[0].pending, "no fresh reading arrived, so nothing was spent");
}

#[test]
fn clicking_a_pending_device_again_does_nothing() {
    let (mut panel, _) = fixture(true, &[("MX Keys", false, true)], clear());
    click(&mut panel, &device_key(0));
    assert!(matches!(click(&mut panel, &device_key(0)), PanelReaction::Repaint), "no second Connect mid-negotiation");
}

/// A device that disappears from the reply — forgotten, or the adapter
/// went down — has no pending state left to show.
#[test]
fn a_vanished_device_settles_its_pending() {
    let (mut panel, _) = fixture(true, &[("MX Keys", false, true)], clear());
    click(&mut panel, &device_key(0));
    panel.set_state(Some("hci0"), &state(true, &[]), clear(), true);
    assert!(panel.view().devices.is_empty());
}

// -- the two-click confirm --------------------------------------------

#[test]
fn forgetting_takes_two_clicks_on_the_same_key() {
    let (mut panel, _) = fixture(true, &[("MX Keys", false, true)], clear());
    panel.tick(Instant::now());

    assert!(matches!(click(&mut panel, &forget_key(0)), PanelReaction::Repaint), "the first click only arms");
    assert!(panel.view().devices[0].armed, "and the row says so on its face");

    let (program, args) = run_args(&click(&mut panel, &forget_key(0)));
    assert_eq!(program, "busctl");
    assert!(args.contains(&"RemoveDevice".to_string()));
    assert!(args.contains(&device_path(0)), "the device is the argument, the adapter is the target");
    assert!(args.contains(&"/org/bluez/hci0".to_string()));
}

#[test]
fn an_arming_expires_on_its_own() {
    let (mut panel, _) = fixture(true, &[("MX Keys", false, true)], clear());
    let now = Instant::now();
    panel.tick(now);
    click(&mut panel, &forget_key(0));
    assert!(panel.view().devices[0].armed);

    assert!(panel.tick(now + FORGET_GRACE), "the disarm is a visible change");
    assert!(!panel.view().devices[0].armed);
}

#[test]
fn a_click_after_the_grace_window_re_arms_rather_than_forgetting() {
    let (mut panel, _) = fixture(true, &[("MX Keys", false, true)], clear());
    let now = Instant::now();
    panel.tick(now);
    click(&mut panel, &forget_key(0));
    // Time passes without a tick that would disarm it, so the second
    // click has to check the clock itself.
    panel.tick(now + FORGET_GRACE + Duration::from_millis(1));
    panel.tick(now + FORGET_GRACE + Duration::from_millis(2));
    assert!(matches!(click(&mut panel, &forget_key(0)), PanelReaction::Repaint), "too late to be a confirm");
    assert!(panel.view().devices[0].armed, "so it is a fresh arming");
}

#[test]
fn anything_else_disarms_the_confirm() {
    let (mut panel, _) = fixture(true, &[("MX Keys", false, true)], clear());
    panel.tick(Instant::now());
    click(&mut panel, &forget_key(0));
    assert!(panel.view().devices[0].armed);

    click(&mut panel, &BtRowKey::PairNew);
    assert!(!panel.view().devices[0].armed, "a click elsewhere is not a confirm");
}

#[test]
fn arming_one_key_disarms_another() {
    let (mut panel, _) = fixture(true, &[("MX Keys", false, true), ("Buds", false, true)], clear());
    panel.tick(Instant::now());
    click(&mut panel, &forget_key(0));
    click(&mut panel, &forget_key(1));
    let view = panel.view();
    assert!(!view.devices.iter().find(|d| d.path == device_path(0)).unwrap().armed);
    assert!(view.devices.iter().find(|d| d.path == device_path(1)).unwrap().armed);
}

/// A press that slides off its control before the release performs
/// nothing — the button contract, which is also what keeps a
/// destructive confirm from firing on a slip.
#[test]
fn a_press_that_changes_its_mind_fires_nothing() {
    let (mut panel, _) = fixture(true, &[("MX Keys", false, true)], clear());
    panel.tick(Instant::now());
    let on_forget = center(&panel, &forget_key(0));
    let elsewhere = center(&panel, &BtRowKey::Power);
    panel.on_event(PanelEvent::LeftPress { local: on_forget }, TILE);
    assert!(matches!(panel.on_event(PanelEvent::LeftRelease { local: elsewhere }, TILE), PanelReaction::Repaint));
    assert!(!panel.view().devices[0].armed, "nothing was armed");
    assert_eq!(panel.view().status, BtStatus::On { connected: 0 }, "and the power row did not fire either");
}

/// The body of a device row and its forget key are different actions,
/// and the boundary is the seam the renderer draws.
#[test]
fn the_forget_key_and_the_row_body_are_different_clicks() {
    let (panel, _) = fixture(true, &[("MX Keys", false, true)], clear());
    let layout = layout(&panel);
    let (bx, by, bw, bh) = layout.row_rect(&device_key(0)).unwrap();
    let (fx, _, fw, _) = layout.row_rect(&forget_key(0)).unwrap();
    assert_eq!(bx + bw as i32, fx, "the body ends exactly where the key begins");
    assert_eq!(layout.row_at(bx + bw as i32 - 1, by + bh as i32 / 2), Some(device_key(0)));
    assert_eq!(layout.row_at(fx + fw as i32 / 2, by + bh as i32 / 2), Some(forget_key(0)));
}

// -- pairing ----------------------------------------------------------

#[test]
fn the_pair_new_row_spawns_the_dialog_and_asks_for_no_resample() {
    let (mut panel, _) = fixture(true, &[], clear());
    match click(&mut panel, &BtRowKey::PairNew) {
        PanelReaction::Run(Effect::Run { program, args, then }) => {
            assert_eq!(program, super::PAIR_DIALOG);
            assert!(args.is_empty());
            assert_eq!(then, None, "the dialog outlives any sample worth hurrying");
        }
        _ => panic!("expected the pairing dialog"),
    }
}

// -- pointer ----------------------------------------------------------

#[test]
fn hover_follows_motion_and_costs_one_repaint_per_change() {
    let (mut panel, _) = fixture(true, &[("MX Keys", false, true)], clear());
    let at = center(&panel, &BtRowKey::Power);
    assert!(matches!(panel.on_event(PanelEvent::Motion { local: at }, TILE), PanelReaction::Repaint));
    assert_eq!(panel.view().hover, Some(BtRowKey::Power));
    assert!(matches!(panel.on_event(PanelEvent::Motion { local: at }, TILE), PanelReaction::None), "same row, no repaint");

    assert!(matches!(panel.on_event(PanelEvent::Leave, TILE), PanelReaction::Repaint));
    assert_eq!(panel.view().hover, None);
    assert!(matches!(panel.on_event(PanelEvent::Leave, TILE), PanelReaction::None));
}

/// A device row's body and its forget key are separate hover targets,
/// so pointing at one never lights the other.
#[test]
fn the_forget_key_hovers_on_its_own() {
    let (mut panel, _) = fixture(true, &[("MX Keys", false, true)], clear());
    panel.on_event(PanelEvent::Motion { local: center(&panel, &forget_key(0)) }, TILE);
    assert_eq!(panel.view().hover, Some(forget_key(0)));
    panel.on_event(PanelEvent::Motion { local: center(&panel, &device_key(0)) }, TILE);
    assert_eq!(panel.view().hover, Some(device_key(0)));
}

// -- repaint economy --------------------------------------------------

/// An open panel costs one boolean per pass and one repaint per actual
/// change — the shape `panel_tick`'s doc prescribes.
#[test]
fn an_idle_panel_asks_for_no_repaints() {
    let (mut panel, bluez) = fixture(true, &[("MX Keys", false, true)], clear());
    let now = Instant::now();
    panel.tick(now);
    let mut fonts = cosmic_text::FontSystem::new();
    let mut swash = cosmic_text::SwashCache::new();
    let theme = wm_theme::default_theme::nextstep_classic();
    let spec = panel.spec(TILE);
    panel.render(&theme, TILE, spec.width, spec.height, &mut fonts, &mut swash);

    for step in 1..8 {
        panel.set_state(Some("hci0"), &bluez, clear(), true);
        assert!(!panel.tick(now + Duration::from_millis(step * 16)), "an unchanged reading changes no pixels");
    }
}

#[test]
fn a_changed_device_list_asks_for_a_repaint() {
    let (mut panel, _) = fixture(true, &[("MX Keys", false, true)], clear());
    let now = Instant::now();
    panel.tick(now);
    let mut fonts = cosmic_text::FontSystem::new();
    let mut swash = cosmic_text::SwashCache::new();
    let theme = wm_theme::default_theme::nextstep_classic();
    let spec = panel.spec(TILE);
    panel.render(&theme, TILE, spec.width, spec.height, &mut fonts, &mut swash);

    panel.set_state(Some("hci0"), &state(true, &[("MX Keys", false, true), ("Buds", true, true)]), clear(), true);
    assert!(panel.tick(now + Duration::from_millis(16)));
}

// -- rendering --------------------------------------------------------

/// The panel renders at the *granted* size, which on a crowded screen
/// is smaller than the one it asked for.
#[test]
fn the_panel_renders_into_a_clamped_grant() {
    let (mut panel, _) = fixture(true, &[("MX Keys", true, true), ("Buds", false, true)], clear());
    let mut fonts = cosmic_text::FontSystem::new();
    let mut swash = cosmic_text::SwashCache::new();
    let theme = wm_theme::default_theme::nextstep_classic();
    let spec = panel.spec(TILE);

    let grant = spec.height / 2;
    let buffer = panel.render(&theme, TILE, spec.width, grant, &mut fonts, &mut swash);
    assert_eq!((buffer.width, buffer.height), (spec.width, grant));
    assert_eq!(buffer.pixels.len(), (spec.width * grant * 4) as usize);
}

/// Every face, at every dock size, in every built-in theme and both
/// appearances. None of this needs a radio, which is the whole point:
/// the states nobody here can reach are still reviewable and still
/// pinned against a panic.
#[test]
fn every_face_renders_at_every_size_in_every_theme() {
    let mut fonts = cosmic_text::FontSystem::new();
    let mut swash = cosmic_text::SwashCache::new();
    let mut panel = BtPanel::new();
    panel.bind(SourceId::from_index(0));

    let devices: &[(&str, bool, bool)] = &[("WH-1000XM4", true, true), ("MX Keys", false, true)];
    let cases: Vec<(Option<&str>, BluezState, RfkillState)> = vec![
        (None, BluezState::default(), RfkillState::default()),
        (Some("hci0"), BluezState::default(), clear()),
        (Some("hci0"), state(false, devices), RfkillState { present: true, soft: true, hard: false }),
        (Some("hci0"), state(false, devices), RfkillState { present: true, soft: true, hard: true }),
        (Some("hci0"), state(true, &[]), clear()),
        (Some("hci0"), state(true, devices), clear()),
    ];

    let themes = wm_theme::default_theme::all_themes()
        .into_iter()
        .chain(wm_theme::default_theme::all_themes_in(wm_theme::model::Appearance::Light));
    for theme in themes {
        for tile in [16u32, 56, 112] {
            for (controller, bluez, rfkill) in &cases {
                panel.set_state(*controller, bluez, *rfkill, true);
                let spec = panel.spec(tile);
                let buffer = panel.render(&theme, tile, spec.width, spec.height, &mut fonts, &mut swash);
                assert_eq!((buffer.width, buffer.height), (spec.width, spec.height), "{} at {tile}", theme.id);
                assert_eq!(buffer.pixels.len(), (spec.width * spec.height * 4) as usize);
            }
        }
    }
}

/// The same view always produces the same pixels — the property that
/// lets a design review of a state this machine cannot reach mean
/// anything at all.
#[test]
fn the_same_view_always_draws_the_same_pixels() {
    let (mut panel, _) = fixture(true, &[("WH-1000XM4", true, true)], clear());
    let mut fonts = cosmic_text::FontSystem::new();
    let mut swash = cosmic_text::SwashCache::new();
    let theme = wm_theme::default_theme::nextstep_classic();
    let spec = panel.spec(TILE);
    let a = panel.render(&theme, TILE, spec.width, spec.height, &mut fonts, &mut swash);
    let b = panel.render(&theme, TILE, spec.width, spec.height, &mut fonts, &mut swash);
    assert_eq!(a.pixels, b.pixels);
}

/// An armed forget key changes the row's pixels — the confirm has to
/// be *visible*, not just remembered.
#[test]
fn arming_a_forget_changes_the_row_it_is_asking_about() {
    let (mut panel, _) = fixture(true, &[("MX Keys", false, true)], clear());
    panel.tick(Instant::now());
    let mut fonts = cosmic_text::FontSystem::new();
    let mut swash = cosmic_text::SwashCache::new();
    let theme = wm_theme::default_theme::nextstep_classic();
    let spec = panel.spec(TILE);
    let calm = panel.render(&theme, TILE, spec.width, spec.height, &mut fonts, &mut swash);

    click(&mut panel, &forget_key(0));
    let armed = panel.render(&theme, TILE, spec.width, spec.height, &mut fonts, &mut swash);
    assert_ne!(calm.pixels, armed.pixels);

    // And the change is on the *row*, not only in the key: the band
    // left of the seam differs too.
    let layout = layout(&panel);
    let (x, y, w, h) = layout.row_rect(&device_key(0)).unwrap();
    let row_changed = (y..y + h as i32).any(|py| {
        (x..x + w as i32).any(|px| {
            let i = ((py as u32 * spec.width + px as u32) * 4) as usize;
            calm.pixels[i..i + 4] != armed.pixels[i..i + 4]
        })
    });
    assert!(row_changed, "the first click must look like a confirm being asked for, at row scale");
}

// -- device classes ---------------------------------------------------

/// BlueZ's `Icon` is a freeform freedesktop name with a long tail; a
/// name this panel has no mark for gets the question mark rather than
/// a plausible guess.
#[test]
fn device_classes_fold_from_bluez_icons() {
    assert_eq!(DeviceClass::from_icon(Some("audio-headset")), DeviceClass::Headset);
    assert_eq!(DeviceClass::from_icon(Some("input-keyboard")), DeviceClass::Keyboard);
    assert_eq!(DeviceClass::from_icon(Some("input-mouse")), DeviceClass::Mouse);
    assert_eq!(DeviceClass::from_icon(Some("phone")), DeviceClass::Phone);
    assert_eq!(DeviceClass::from_icon(Some("printer")), DeviceClass::Unknown, "no mark is better than the wrong mark");
    assert_eq!(DeviceClass::from_icon(None), DeviceClass::Unknown);
}

#[test]
fn the_battery_meter_reads_the_whole_range() {
    assert_eq!(render::battery_level(0), 0.0);
    assert_eq!(render::battery_level(100), 1.0);
    assert_eq!(render::battery_level(200), 1.0, "a reading past full clamps rather than overflowing the track");
    assert!(render::battery_level(50) > render::battery_level(49));
}
