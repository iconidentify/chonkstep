//! The dialog's face, pinned. Every one of these runs on a machine
//! with no X server, no BlueZ and no radio, which is the machine this
//! window was written on — [`draw`] is a pure function of a theme, a
//! phase and a scale, and that is the whole reason it can be reviewed
//! at all here.

use super::*;
use crate::pair::Found;

fn found(address: &str, paired: bool) -> Found {
    Found { address: address.to_string(), name: address.to_string(), paired }
}

fn theme() -> Theme {
    chonk_ui::nextstep_theme()
}

/// Every phase must produce a paintable window at every scale this
/// session can be in — including the one this machine reaches, which
/// is `Unavailable`.
#[test]
fn every_phase_paints_at_every_scale() {
    let theme = theme();
    let mut fonts = cosmic_text::FontSystem::new();
    let mut swash = cosmic_text::SwashCache::new();
    let devices = [found("AA:BB:CC:DD:EE:FF", false), found("F8:4E:17:00:11:22", true)];
    let refs: Vec<&Found> = devices.iter().collect();
    for scale in [1.0f32, 2.0] {
        let m = Metrics::new(scale);
        for phase in &all_phases() {
            let mut pixmap = Pixmap::new(m.width, m.height).expect("nonzero window");
            draw(&mut pixmap, &theme, &mut fonts, &mut swash, &m, phase, &refs, None);
            assert_eq!((pixmap.width(), pixmap.height()), (m.width, m.height), "{phase:?} at {scale}x");
        }
    }
}

fn all_phases() -> Vec<Phase> {
    vec![
        Phase::Starting,
        Phase::Scanning,
        Phase::Pairing { address: "AA:BB:CC:DD:EE:FF".into() },
        Phase::Confirm { address: "AA:BB:CC:DD:EE:FF".into(), passkey: "123456".into() },
        Phase::DisplayPasskey { address: "AA:BB:CC:DD:EE:FF".into(), passkey: "042318".into() },
        Phase::NeedsKeyboard { address: "AA:BB:CC:DD:EE:FF".into() },
        Phase::Paired { address: "AA:BB:CC:DD:EE:FF".into() },
        Phase::Failed { address: "AA:BB:CC:DD:EE:FF".into(), reason: "AuthenticationFailed".into() },
        Phase::Unavailable { reason: "no Bluetooth controller".into() },
    ]
}

/// A click resolves to the row it was drawn on, and an already-paired
/// device is not offered again.
#[test]
fn the_list_hit_test_matches_the_rows_it_draws() {
    let m = Metrics::new(1.0);
    let devices = [found("AA:BB:CC:DD:EE:FF", false), found("F8:4E:17:00:11:22", true)];
    let refs: Vec<&Found> = devices.iter().collect();
    let hits = layout(&m, &Phase::Scanning, &refs);
    assert_eq!(hits.hits.len(), 1, "a paired device stays visible but is not a target");

    let (x, y, w, h) = hits.hits[0].rect;
    assert_eq!(hits.at(x + w as i32 / 2, y + h as i32 / 2), Some(&Target::Device("AA:BB:CC:DD:EE:FF".to_string())));
    assert_eq!(hits.at(x - 2, y + 1), None, "outside the row is chrome");
    assert_eq!(hits.at(x + 1, y - 2), None);
}

/// Every list row lands inside the well it is drawn in — the defect a
/// re-skin most easily introduces is a row that hangs a pixel over the
/// sunken lip and takes clicks that look like chrome.
#[test]
fn every_list_row_lands_inside_the_well() {
    let m = Metrics::new(2.0);
    let devices: Vec<Found> = (0..MAX_ROWS).map(|i| found(&format!("AA:BB:CC:DD:EE:{i:02X}"), false)).collect();
    let refs: Vec<&Found> = devices.iter().collect();
    let (wx, wy, ww, wh) = well_rect(&m);
    for hit in layout(&m, &Phase::Scanning, &refs).hits {
        let (x, y, w, h) = hit.rect;
        assert!(x >= wx && x + w as i32 <= wx + ww as i32, "{hit:?} is outside the well horizontally");
        assert!(y >= wy && y + h as i32 <= wy + wh as i32, "{hit:?} is outside the well vertically");
    }
}

/// The footer's two answers sit where the join dialog's two buttons
/// sit: the action on the right, the way out to its left, both on the
/// bottom margin. This is the assertion that keeps the two spawned
/// dialogs looking like one desktop as either of them is edited.
#[test]
fn the_confirm_phase_puts_its_answers_in_the_join_dialogs_button_slots() {
    let m = Metrics::new(1.0);
    let hits = layout(&m, &Phase::Confirm { address: "A".into(), passkey: "123456".into() }, &[]);
    assert_eq!(hits.hits.len(), 2);
    let yes = hits.hits.iter().find(|h| h.target == Target::Yes).expect("yes");
    let no = hits.hits.iter().find(|h| h.target == Target::No).expect("no");
    assert!(no.rect.0 + no.rect.2 as i32 <= yes.rect.0, "the two answers must not overlap");
    assert!(yes.rect.0 > no.rect.0, "the action is the right-hand button, as it is next door");
    assert_eq!(yes.rect.1, no.rect.1, "both on the same footer line");
    assert_eq!(
        yes.rect.0 + yes.rect.2 as i32,
        m.width as i32 - MARGIN,
        "the action button is flush with the right margin"
    );
    assert_eq!(yes.rect.1 + yes.rect.3 as i32, m.height as i32 - MARGIN, "and with the bottom margin");
    assert_eq!(hits.at(yes.rect.0 + 2, yes.rect.1 + 2), Some(&Target::Yes));
    assert_eq!(hits.at(no.rect.0 + 2, no.rect.1 + 2), Some(&Target::No));
}

/// The phases with nothing to answer must offer nothing to click — a
/// button that cannot work is worse than no button.
#[test]
fn the_waiting_phases_offer_no_targets() {
    let m = Metrics::new(1.0);
    for phase in [
        Phase::Pairing { address: "A".into() },
        Phase::DisplayPasskey { address: "A".into(), passkey: "1".into() },
        Phase::Unavailable { reason: "no Bluetooth controller".into() },
    ] {
        assert!(layout(&m, &phase, &[]).hits.is_empty(), "{phase:?} must offer nothing");
    }
}

#[test]
fn the_finished_phases_all_offer_the_way_back() {
    let m = Metrics::new(1.0);
    for phase in [
        Phase::Paired { address: "A".into() },
        Phase::Failed { address: "A".into(), reason: "x".into() },
        Phase::NeedsKeyboard { address: "A".into() },
    ] {
        let hits = layout(&m, &phase, &[]);
        assert_eq!(hits.hits.len(), 1);
        assert_eq!(hits.hits[0].target, Target::Rescan, "{phase:?}");
    }
}

/// A long list is capped rather than drawn off the well.
#[test]
fn a_crowded_room_caps_the_list() {
    let m = Metrics::new(1.0);
    let devices: Vec<Found> = (0..20).map(|i| found(&format!("AA:BB:CC:DD:EE:{i:02X}"), false)).collect();
    let refs: Vec<&Found> = devices.iter().collect();
    assert_eq!(layout(&m, &Phase::Scanning, &refs).hits.len(), MAX_ROWS);
}

/// The well's paper is the join dialog's, and it has to keep that
/// recipe's one promise in every theme and both appearances: the well
/// moves *away* from the surface's ink, never toward it. Mixing toward
/// the ink is the obvious first spelling and produces a well that
/// differs from the panel by a few percent and vanishes — the defect
/// the join dialog's own design pass caught, restated here so a future
/// tidy-up of this file cannot reintroduce it.
#[test]
fn the_well_reads_as_paper_in_every_theme() {
    let themes = wm_theme::default_theme::all_themes()
        .into_iter()
        .chain(wm_theme::default_theme::all_themes_in(wm_theme::model::Appearance::Light));
    for theme in themes {
        let paper = luminance(field_tone(&theme));
        let surface = luminance(fill_tone(&theme.menu.background));
        let ink = luminance(theme.menu.text_color);
        assert!(
            (paper - ink).abs() > (surface - ink).abs(),
            "theme {} ({:?}): the well must sit further from the ink than the surface does — paper {paper:.0}, surface {surface:.0}, ink {ink:.0}",
            theme.id,
            theme.appearance
        );
        assert!((paper - ink).abs() > 96.0, "theme {}: the surface's ink must read on the well's paper", theme.id);
    }
}

/// Hover changes the pixels of the row it is on — and only after the
/// pointer is actually on one.
#[test]
fn hovering_a_row_changes_that_rows_pixels() {
    let theme = theme();
    let mut fonts = cosmic_text::FontSystem::new();
    let mut swash = cosmic_text::SwashCache::new();
    let m = Metrics::new(1.0);
    let devices = [found("AA:BB:CC:DD:EE:FF", false)];
    let refs: Vec<&Found> = devices.iter().collect();

    let render = |hover: Option<&Target>, fonts: &mut cosmic_text::FontSystem, swash: &mut cosmic_text::SwashCache| {
        let mut pixmap = Pixmap::new(m.width, m.height).expect("nonzero window");
        draw(&mut pixmap, &theme, fonts, swash, &m, &Phase::Scanning, &refs, hover);
        pixmap.data().to_vec()
    };
    let calm = render(None, &mut fonts, &mut swash);
    let hot = render(Some(&Target::Device("AA:BB:CC:DD:EE:FF".to_string())), &mut fonts, &mut swash);
    assert_ne!(calm, hot);
    // And it is byte-stable, which is what lets a design review of a
    // phase no one here can reach mean anything.
    assert_eq!(calm, render(None, &mut fonts, &mut swash));
}
