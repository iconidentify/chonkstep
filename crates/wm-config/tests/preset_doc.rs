//! The Omarchy keymap's documentation has to be the keymap.
//!
//! `docs/keybindings.md` carries all 93 bindings and all 37 unbound
//! chord groups as tables. A table transcribed by hand once is a table
//! that is wrong the second time someone edits the preset — and unlike
//! a stale comment, a stale keybinding card is *actively harmful*: the
//! whole reason the Omarchy keymap exists is that a user who cannot
//! find a binding bounces, and a card that names a chord the preset no
//! longer binds sends them looking for a key that does nothing.
//!
//! So the tables are pinned in both directions. Every binding in the
//! preset must appear in the card, *and* every row in the card's
//! Omarchy table must be a binding in the preset. Neither list can
//! grow, shrink or be edited alone.

use std::collections::BTreeSet;
use wm_config::preset::{OMARCHY_BINDINGS, OMARCHY_COMMANDS, OMARCHY_UNBOUND};

const CARD: &str = include_str!("../../../docs/keybindings.md");
const REFERENCE: &str = include_str!("../../../docs/config.example.toml");
const MODE: &str = include_str!("../../../docs/omarchy-mode.md");

/// The card's Omarchy binding table: the section between the heading
/// and the unbound one, as `(binding, action, command)` from each row.
fn card_binding_rows() -> Vec<(String, String, String)> {
    let section = CARD
        .split_once("## The Omarchy keymap")
        .expect("the card must have an Omarchy keymap section")
        .1
        .split_once("### Deliberately unbound")
        .expect("...followed by the unbound one")
        .0;
    table_rows(section)
}

/// The card's unbound table, from the heading to the next `##`.
fn card_unbound_rows() -> Vec<(String, String, String)> {
    let after = CARD.split_once("### Deliberately unbound").expect("an unbound section").1;
    let section = after.split_once("\n## ").map_or(after, |(head, _)| head);
    table_rows(section)
}

/// Three-column markdown rows whose first cell is code-quoted, with
/// the backticks and padding stripped. The `|---|` separator and the
/// `| Binding | ... |` header both fail the first-cell test, so they
/// drop out without being special-cased.
fn table_rows(section: &str) -> Vec<(String, String, String)> {
    section
        .lines()
        .filter_map(|line| {
            // Split on unescaped `|` only: a cell may legitimately carry
            // a `\|`, and treating that as a boundary would silently
            // drop the two rows whose command line is a shell `||`.
            let body = line.trim().strip_prefix('|')?.strip_suffix('|')?;
            let cells: Vec<String> = split_cells(body);
            let [first, second, third] = cells.as_slice() else { return None };
            let unquote = |cell: &str| cell.trim().trim_matches('`').to_string();
            first
                .trim_start()
                .starts_with('`')
                .then(|| (unquote(first), unquote(second), unquote(third)))
        })
        .collect()
}

/// A markdown row body split on unescaped `|`, keeping any `\|` in
/// the cell it belongs to.
fn split_cells(body: &str) -> Vec<String> {
    let mut cells = vec![String::new()];
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cells.last_mut().expect("never empty").push('\\');
                cells.last_mut().expect("never empty").push(chars.next().expect("peeked"));
            }
            '|' => cells.push(String::new()),
            other => cells.last_mut().expect("never empty").push(other),
        }
    }
    cells
}

/// The argv column as the card prints it: shell-ish, with only the
/// arguments that need quoting quoted, so an argument containing `||`
/// reads as the one word it is — and with the pipes then escaped,
/// because an unescaped `|` inside a markdown cell *is* a cell
/// boundary and would split the row in two.
fn printed_argv(argv: &[&str]) -> String {
    argv.iter()
        .map(|arg| if arg.contains(' ') || arg.contains('|') { format!("\"{arg}\"") } else { (*arg).to_string() })
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}

#[test]
fn the_card_lists_exactly_the_bindings_the_preset_holds() {
    let commands: std::collections::BTreeMap<&str, &[&str]> = OMARCHY_COMMANDS.iter().copied().collect();
    let expected: Vec<(String, String, String)> = OMARCHY_BINDINGS
        .iter()
        .map(|(spec, action)| {
            let argv = action.strip_prefix("run ").map(|name| {
                printed_argv(commands.get(name).unwrap_or_else(|| panic!("{name} must be declared")))
            });
            (spec.to_string(), action.to_string(), argv.unwrap_or_else(|| "--".to_string()))
        })
        .collect();

    let rows = card_binding_rows();
    // Row order too: the card is grouped by the Omarchy file each chord
    // comes from, which is only useful if it matches the source's order.
    assert_eq!(
        rows,
        expected,
        "docs/keybindings.md's Omarchy table and wm_config::preset::OMARCHY_BINDINGS disagree; \
         regenerate the table from the source"
    );
}

#[test]
fn the_card_lists_exactly_the_chords_the_preset_leaves_unbound() {
    let expected: Vec<(String, String, String)> = OMARCHY_UNBOUND
        .iter()
        .map(|(chords, what, why)| (chords.to_string(), what.to_string(), why.reason().replace("--", "\u{2014}")))
        .collect();
    assert_eq!(
        card_unbound_rows(),
        expected,
        "docs/keybindings.md's unbound table and wm_config::preset::OMARCHY_UNBOUND disagree"
    );
}

/// Prose counts drift too, and a wrong one is the kind of small lie
/// that costs a reader their trust in the rest of the page.
#[test]
fn the_counts_quoted_in_prose_are_the_real_counts() {
    let bindings = OMARCHY_BINDINGS.len().to_string();
    let commands = OMARCHY_COMMANDS.len().to_string();
    let unbound = OMARCHY_UNBOUND.len().to_string();
    for (doc, name) in [(CARD, "docs/keybindings.md"), (MODE, "docs/omarchy-mode.md")] {
        assert!(doc.contains(&format!("{bindings} bindings")), "{name} does not say \"{bindings} bindings\"");
        assert!(doc.contains(&format!("{commands} ")), "{name} does not mention {commands} commands");
        assert!(doc.contains(&unbound), "{name} does not mention {unbound} unbound groups");
    }
}

/// Every key token the reference lists has to be a real name — the
/// same rule `example_doc.rs` applies to the media keys, extended to
/// the ones the Omarchy keymap needed added.
#[test]
fn the_key_tokens_the_omarchy_keymap_needed_are_documented_and_real() {
    for name in ["print", "bracketleft", "bracketright", "kbdlightonoff", "calculator", "eject"] {
        assert!(
            wm_config::parse_key(name).is_some(),
            "the Omarchy keymap uses {name} but the parser does not know it"
        );
        assert!(wm_config::parse_key(&format!("super+alt+{name}")).is_some(), "super+alt+{name}");
        assert!(
            REFERENCE.contains(name),
            "docs/config.example.toml must list {name} among the key tokens a reader can copy"
        );
    }
}

/// The reference's own summary of what the posture sets, checked
/// against what it actually sets. This is the table a user reads
/// before typing one line and logging out.
#[test]
fn the_reference_summarises_the_posture_correctly() {
    let config = wm_config::parse("desktop = \"omarchy\"").expect("the documented one-liner must parse");
    let claims = [
        ("show_dock    = false", !config.show_dock),
        ("omarchy_bar  = true", config.omarchy_bar == Some(true)),
        ("theme        = \"omarchy\"", config.theme.as_deref() == Some("omarchy")),
        ("keymap        = \"omarchy\"", config.keymap == wm_config::preset::Keymap::Omarchy),
        ("omarchy_menu  = true", config.omarchy_menu),
        ("omarchy_shell = true", config.omarchy_shell),
    ];
    for (line, holds) in claims {
        assert!(REFERENCE.contains(line), "docs/config.example.toml must show the posture's `{line}`");
        assert!(holds, "the reference claims `{line}` but the preset does not do it");
    }
    // ...and the reference's promise that it sets nothing else. The
    // keys it names as untouched, each still at its built-in default.
    let default = wm_config::Config::default_config();
    assert_eq!(config.terminal, default.terminal, "the posture must not set `terminal`");
    assert_eq!(config.autostart, default.autostart, "...nor `autostart`");
    assert_eq!(config.lock_command, default.lock_command, "...nor `lock_command`");
    assert_eq!(config.placement, default.placement, "...nor `placement`");
    assert_eq!(config.focus_follows_mouse, default.focus_follows_mouse, "...nor the focus policy");
    assert_eq!(config.scale, default.scale, "...nor `scale`");
    assert_eq!(config.drag_modifier, default.drag_modifier, "...nor `drag_modifier`");
    assert_eq!(config.edge_resistance, default.edge_resistance, "...nor `edge_resistance`");
    assert_eq!(config.decorations.client_side, default.decorations.client_side, "...nor the decoration rules");
    assert_eq!(config.decorations.server_side, default.decorations.server_side);
    assert_eq!(config.terminal_font_px, default.terminal_font_px, "...nor `terminal_font_px`");
    assert_eq!(config.restore_session, default.restore_session, "...nor `restore_session`");
    assert_eq!(config.appearance, default.appearance, "...nor `appearance`");
}

/// The example both preset keys are documented with, parsed. The
/// reference is a file people paste out of.
#[test]
fn the_documented_preset_names_all_parse() {
    for name in ["chonkstep", "omarchy"] {
        let config = wm_config::parse(&format!("desktop = {name:?}")).unwrap();
        assert_eq!(config.desktop.id(), name);
        let config = wm_config::parse(&format!("keymap = {name:?}")).unwrap();
        assert_eq!(config.keymap.id(), name);
    }
    // And the combination the card and the mode page both suggest.
    let config = wm_config::parse("desktop = \"omarchy\"\nkeymap = \"chonkstep\"").unwrap();
    assert_eq!(config.keymap.id(), "chonkstep");
    assert!(!config.show_dock);
}

/// Nothing in either page names a chord that is in both tables: a
/// chord cannot be bound and documented dead at once. Checked over the
/// unbound entries that name one literal chord, since the rest name
/// families.
#[test]
fn no_chord_is_both_bound_and_documented_unbound() {
    let bound: BTreeSet<&str> = OMARCHY_BINDINGS.iter().map(|(spec, _)| *spec).collect();
    for (chords, _, _) in OMARCHY_UNBOUND {
        for chord in chords.split(" / ") {
            let chord = chord.trim();
            assert!(
                !bound.contains(chord),
                "{chord} is in OMARCHY_BINDINGS and in OMARCHY_UNBOUND at the same time"
            );
        }
    }
}
