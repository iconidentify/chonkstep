//! The unbounded subsurface tree, spelled as assertions.
//!
//! # The defect
//!
//! `wl_subcompositor.get_subsurface` rejects self-parenting and cycles
//! and nothing else, so how *deep* a client nests its surfaces was a
//! number the client chose. Every walk of that tree is recursive — two
//! of them run on an ordinary commit, a third under every scene build,
//! bounding box, frame callback and presentation feedback the
//! compositor performs — and the stack they run on is 8 MiB. Tens of
//! thousands of `wl_surface`/`wl_subsurface` pairs is a few hundred
//! thousand protocol messages and a fraction of a second's work for a
//! client; it is also a `SIGSEGV` against the guard page for the
//! compositor, which runs no panic hook, restores no gamma ramp,
//! unlinks no socket and names no culprit in the log. An unprivileged
//! client ended the session and every application in it, and left the
//! supervisor with "compositor exited abnormally".
//!
//! # What these tests drive
//!
//! `chonk-subsurface-probe`, in both halves (see its module doc). The
//! hostile half builds its chain in both constructions a client can
//! use, because they are not equally easy to catch: growing the chain
//! *leaf-first* presents a parent with no parent of its own on every
//! call, so a depth counter that measures only from the new child up to
//! its root reads zero on every link of a chain of any length. The
//! honest half exists for the other half of the fix — the compositor's
//! own scene walk became iterative, and a subsurface's z-position
//! relative to its parent is exactly what that rewrite could silently
//! lose.

use std::time::Duration;

use chonk_testkit::{near, poll_until, profile_binary, Screenshot, Session, SessionOptions, WindowInfo};
use wm_theme_api::MAX_SUBSURFACE_DEPTH;

/// The probe's three colours, as a screenshot sees them.
const PARENT_RGB: [u8; 3] = [0xE0, 0x20, 0x20];
const OVER_RGB: [u8; 3] = [0x20, 0x20, 0xE0];
const UNDER_RGB: [u8; 3] = [0x20, 0xE0, 0x20];

/// Both stacking facts the probe's window encodes, checked at once.
///
/// The corner belongs to the subsurface placed *above* the parent and
/// the middle to the parent itself, which sits above a green sheet of
/// its own size. Green anywhere means a below-subsurface was drawn over
/// its parent; red in the corner means an above-subsurface was drawn
/// under it. Either is the ordering bug the iterative scene walk had to
/// avoid, and neither is visible from a window's geometry.
fn assert_subsurface_stacking(shot: &Screenshot, window: &WindowInfo, when: &str) {
    let (x, y) = (window.x.max(0) as u32, window.y.max(0) as u32);
    let corner = shot.mean_rgb(x + 16, y + 16, 16, 16);
    assert!(
        near(corner, OVER_RGB),
        "{when}: the subsurface placed above the parent must own the window's corner, \
         got {corner:?} (parent {PARENT_RGB:?}, above {OVER_RGB:?}) in {}",
        shot.path.display()
    );
    let middle = shot.mean_rgb(x + window.w / 2 - 8, y + window.h / 2 - 8, 16, 16);
    assert!(
        near(middle, PARENT_RGB),
        "{when}: the parent must stay above the subsurface placed below it, \
         got {middle:?} (parent {PARENT_RGB:?}, below {UNDER_RGB:?}) in {}",
        shot.path.display()
    );
}

/// Waits for a line in one launched client's log.
fn wait_for_client_line(session: &Session, log_name: &str, needle: &str) {
    let path = session.dir.join(log_name);
    poll_until(Duration::from_secs(15), &format!("{log_name} to say {needle:?}"), || {
        std::fs::read_to_string(&path).ok().filter(|text| text.contains(needle)).map(|_| ())
    })
    .unwrap_or_else(|timeout| {
        panic!("{timeout}\n{}", std::fs::read_to_string(&path).unwrap_or_default())
    });
}

/// The load-bearing regression, in one session because the point of it
/// is that the three clients share one.
///
/// The honest client maps first and is photographed before and after
/// the two hostile ones run. On the unfixed compositor the second
/// photograph never happens: the session is gone.
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn an_over_deep_subsurface_chain_kills_its_own_client_and_nothing_else() {
    let probe = profile_binary("chonk-subsurface-probe")
        .expect("cargo build -p chonk-testkit builds the probe");
    let probe = probe.to_string_lossy().to_string();
    let mut session = Session::boot(
        "bounded-subsurface-depth",
        SessionOptions { scale: Some(1.0), ..SessionOptions::default() },
    )
    .expect("the nested compositor boots");

    session
        .launch(&probe, &["stack", "subsurface-stack"])
        .expect("the honest probe launches");
    let window = session
        .wait_for_window("subsurface-stack")
        .expect("the honest probe maps its toplevel");
    assert!(
        window.w >= 200 && window.h >= 200,
        "the probe's window is the canvas these assertions sample: {window:?}"
    );
    session.door().barrier().expect("the compositor settles");
    let before = session.screenshot("subsurface-stack-before").unwrap();
    assert_subsurface_stacking(&before, &window, "before the hostile clients");

    // Both constructions, one after the other. Each is accepted up to
    // the ceiling and refused one link past it.
    let limit = MAX_SUBSURFACE_DEPTH.to_string();
    session
        .launch(&probe, &["deep-chain", &limit])
        .expect("the leaf-first probe launches");
    session
        .launch(&probe, &["deep-chain-root", &limit])
        .expect("the root-first probe launches");

    for (log, order) in [
        ("client-1-chonk-subsurface-probe.log", "leaf-first"),
        ("client-2-chonk-subsurface-probe.log", "root-first"),
    ] {
        wait_for_client_line(
            &session,
            log,
            &format!("**{order} chain of {MAX_SUBSURFACE_DEPTH} links accepted**"),
        );
        wait_for_client_line(
            &session,
            log,
            &format!("**{order} refused at {} links:", MAX_SUBSURFACE_DEPTH + 1),
        );
        // A refusal that does not disconnect leaves the over-deep chain
        // standing in the tree, and the next commit walks it.
        wait_for_client_line(&session, log, "**connection closed:");
    }

    session
        .door()
        .barrier()
        .expect("the compositor answers after both hostile clients have run");
    let log = session.log();
    assert_eq!(
        log.matches("client subsurface tree exceeded the compositor's depth limit")
            .count(),
        2,
        "one diagnostic per offending client, naming the depth it asked for:\n{log}"
    );

    // The other half of "nothing else": the innocent client kept its
    // window, its pixels, and its stacking order.
    let after = session
        .wait_for_window("subsurface-stack")
        .expect("the honest client outlives the hostile ones");
    let shot = session.screenshot("subsurface-stack-after").unwrap();
    assert_subsurface_stacking(&shot, &after, "after the hostile clients");
}
