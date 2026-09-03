//! The workspace-by-number chords, end to end: `workspace <n>` and
//! `workspace-carry <n>` on a real session, with a real client, driven
//! by real key presses.
//!
//! # Why this test and not a unit test
//!
//! The arithmetic is pinned in three places already — the parser turns
//! the file's 1-based number into an index (`wm_config`), the window
//! manager acts on that index (`wm_core::manager`), and the preset's
//! table binds the chords (`wm_config::preset`). Each of those is
//! internally consistent with itself while being off by one from the
//! others, and every one of the unit tests would still pass. The only
//! thing that can catch a disagreement *between* the vocabularies is a
//! session where the key is pressed and the desk is looked at, because
//! only there does "the first workspace" mean the one the user is
//! already standing on.
//!
//! So the load-bearing assertion here is the boring one: pressing the
//! chord for workspace **1** on a freshly booted desk does *nothing*.
//! If the number ever reaches `switch_workspace` unconverted, that
//! press moves to the second workspace and the window vanishes — which
//! is exactly the bug an Omarchy user would report as "super+1 goes to
//! the wrong desk" six months from now.
//!
//! Same run rules as `e2e.rs`: needs a live session to nest in, so
//! `#[ignore]`d; run with `scripts/e2e.sh` or
//! `cargo test -p chonk-testkit -- --ignored --test-threads=1`.

use std::time::Duration;

use chonk_testkit::{keys, poll_until, Session, SessionOptions, World};

/// Long enough for a key press to travel through the compositor and
/// for the map or unmap it causes to reach the door's ledger.
const SETTLE: Duration = Duration::from_secs(10);

/// `super+<digit>`, and `super+shift+<digit>` for the carry — two
/// modifiers, so the door's single-modifier `chord` does not fit.
fn workspace_chord(session: &mut Session, shift: bool, digit: u32) {
    let door = session.door();
    door.key(keys::LEFTMETA, true).unwrap();
    if shift {
        door.key(keys::LEFTSHIFT, true).unwrap();
    }
    door.barrier().unwrap();
    door.tap_key(digit).unwrap();
    if shift {
        door.key(keys::LEFTSHIFT, false).unwrap();
    }
    door.key(keys::LEFTMETA, false).unwrap();
    door.barrier().unwrap();
}

/// Is the terminal on the screen right now?
///
/// The *frame's* mapped flag, not the window's: parking a window on
/// another workspace unmaps the decoration the compositor drew around
/// it and leaves the client's own surface alone — the client is never
/// told it went away, and its record stays `mapped=true` throughout.
/// A test that asked the window record would see no difference between
/// the two workspaces at all. The fallback to the window's own flag is
/// for a client that negotiated its own decorations and has no frame
/// of ours to hide.
fn on_screen(world: &World, needle: &str) -> bool {
    world
        .windows
        .iter()
        .filter(|w| w.app.contains(needle) || w.title.contains(needle))
        .any(|w| w.mapped && world.frame_of(w.id).map_or(w.mapped, |frame| frame.mapped))
}

/// Waits until the terminal is on screen, or off it.
fn wait_until_on_screen(session: &mut Session, visible: bool, what: &str) {
    poll_until(SETTLE, what, || {
        let world = session.world().ok()?;
        (on_screen(&world, "foot") == visible).then_some(())
    })
    .unwrap_or_else(|e| panic!("{what}: {e}"));
}

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn the_workspace_chords_count_from_one_and_carry_the_window_along() {
    // The Omarchy chords, spelled the way a user would spell them in
    // their own file rather than pulled from the preset: this test is
    // about the vocabulary, and the preset is one caller of it.
    let config = "[keybindings]\n\
                  \"super+1\" = \"workspace 1\"\n\
                  \"super+2\" = \"workspace 2\"\n\
                  \"super+3\" = \"workspace 3\"\n\
                  \"super+shift+2\" = \"workspace-carry 2\"\n"
        .to_string();
    let mut session = Session::boot("workspace-by-index", SessionOptions { config_extra: config, ..Default::default() })
        .expect("session boots");

    session.launch("foot", &[]).expect("foot launches");
    session.wait_for_window("foot").expect("foot maps");

    // The one that catches the off-by-one: workspace 1 is where the
    // session starts, so this chord is a no-op and the terminal stays
    // exactly where it is. A 0-based reading would move the desk to
    // the second workspace and take the terminal off the screen.
    workspace_chord(&mut session, false, keys::ONE);
    wait_until_on_screen(&mut session, true, "the terminal to stay put for a switch to the workspace it is on");
    let window = session.world().unwrap().window_matching("foot").expect("still on screen").id;

    // The carry, while the terminal still holds the focus it was
    // mapped with. Workspace 2 does not exist yet; naming it creates
    // it, and the window arrives with the desk rather than being left
    // behind.
    workspace_chord(&mut session, true, keys::TWO);
    wait_until_on_screen(&mut session, true, "the carried terminal to arrive on workspace 2");

    // ...and it really moved rather than being drawn over both: the
    // workspace it came from is empty now.
    workspace_chord(&mut session, false, keys::ONE);
    wait_until_on_screen(&mut session, false, "workspace 1 to be empty after the carry");

    // Back to where it went, and it is the same window throughout —
    // carried, never re-created.
    workspace_chord(&mut session, false, keys::TWO);
    wait_until_on_screen(&mut session, true, "the terminal to be waiting on workspace 2");
    assert_eq!(session.world().unwrap().window_matching("foot").unwrap().id, window, "the carried window is the same window");

    // Growth from the far end: workspace 3 has never been visited, and
    // asking for it is a fresh empty desk, not an error.
    workspace_chord(&mut session, false, keys::THREE);
    wait_until_on_screen(&mut session, false, "workspace 3 to be a fresh empty desk");

    // A carry with nothing focused does nothing at all — including not
    // switching. This desk is empty (a workspace switch leaves nothing
    // focused), so if the verb travelled anyway it would land on
    // workspace 2 and the terminal would appear.
    workspace_chord(&mut session, true, keys::TWO);
    wait_until_on_screen(&mut session, false, "a carry with nothing in hand to leave the desk alone");
}
