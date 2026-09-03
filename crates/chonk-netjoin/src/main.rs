//! `chonk-netjoin <ssid>` — the join dialog's process.
//!
//! This file is the whole impure surface of the crate: the X
//! connection, and the one `Command`. Everything it decides with is
//! computed by [`chonk_netjoin::dialog`], which knows about neither.
//! See the crate doc for why the passphrase goes down a pipe and never
//! into an argument list.

use chonk_netjoin::dialog::{Action, JoinDialog};
use chonk_netjoin::render::{layout, render_join_dialog};

// The window belongs to the binary, not the library: it is the one
// part of this crate that cannot be exercised headless, and a library
// that exported an X connection would invite someone to link it from a
// process that promised not to hold one (a dockapp, say).
mod window;
use window::{Input, Window};

fn main() {
    // One positional argument, the SSID — the contract the link panel
    // calls with (`Effect::Run { program: "chonk-netjoin", args: vec![ssid] }`).
    let Some(ssid) = std::env::args().nth(1).filter(|s| !s.is_empty()) else {
        eprintln!("usage: chonk-netjoin <ssid>");
        std::process::exit(2);
    };

    // The desk's own clothes, if the launcher passed them along. The
    // effect executor that spawns this does not currently inject
    // `CHONKSTEP_THEME`/`CHONKSTEP_APPEARANCE` (see docs/link-panel.md),
    // so today this usually resolves to the flagship theme — which is
    // a wrong-but-coherent look, not a broken one, and becomes correct
    // for free the moment the executor carries the environment.
    let scale = chonk_ui::scale_factor();
    let theme = chonk_ui::scaled_theme();
    let l = layout(scale);

    let Some(window) = Window::open(&format!("Join {ssid}"), l.width, l.height) else {
        eprintln!("chonk-netjoin: cannot open a window (no X display?)");
        std::process::exit(1);
    };

    let mut fonts = cosmic_text::FontSystem::new();
    let mut swash = cosmic_text::SwashCache::new();
    let mut dialog = JoinDialog::new(ssid);

    loop {
        let pixmap = render_join_dialog(&theme, &mut fonts, &mut swash, scale, &dialog.view());
        window.present(&pixmap);

        let action = match window.next() {
            Input::Closed => return,
            Input::Redraw => None,
            Input::Key(key) => dialog.on_key(key),
            Input::Press { x, y } => {
                dialog.on_press(x, y, &l);
                None
            }
            Input::Release { x, y } => dialog.on_release(x, y, &l),
        };

        match action {
            Some(Action::Close) => return,
            Some(Action::Join { ssid, passphrase }) => {
                // Paint the "joining…" face before blocking on nmcli,
                // or the dialog would sit on its old pixels for the
                // several seconds an association takes and read as
                // hung. This is the one place the loop draws out of
                // turn, and the reason it is worth it.
                let pixmap = render_join_dialog(&theme, &mut fonts, &mut swash, scale, &dialog.view());
                window.present(&pixmap);

                let (ok, reason) = join(&ssid, &passphrase);
                dialog.finished(ok, &reason);
            }
            None => {}
        }
    }
}

/// Runs the join, with the passphrase on stdin and nowhere else.
///
/// Returns whether it worked and, if not, nmcli's stderr — raw, for
/// [`clean_reason`] to reduce. stdout is discarded rather than
/// captured: `nmcli --ask` reading a prompt from a pipe has no
/// terminal on which to disable echo, so stdout is the one stream the
/// passphrase could conceivably reappear on, and the simplest way not
/// to show it is not to have it.
fn join(ssid: &str, passphrase: &str) -> (bool, String) {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let child = Command::new("nmcli")
        .arg("--ask")
        .args(["device", "wifi", "connect"])
        // `--` first: an SSID may legitimately begin with a dash, and
        // without the separator nmcli would read it as an option.
        .arg("--")
        .arg(ssid)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        // A missing nmcli is the one failure with a useful thing to
        // say, since it means this desktop cannot join anything.
        Err(err) => return (false, format!("cannot run nmcli: {err}")),
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = writeln!(stdin, "{passphrase}");
        // Dropping the pipe closes it, which is what stops nmcli
        // waiting for a second prompt it will not get.
    }

    // Audited exception to the workspace `clippy.toml`'s ban on
    // `Command::output`: the banned case is a compositor thread that
    // must never park, and this is a dedicated dialog process whose
    // entire purpose is to wait for this command. Nothing repaints
    // behind it — the "joining…" face is already on screen — and the
    // worst case is a window that stays up until nmcli's own timeout.
    #[allow(clippy::disallowed_methods)]
    let done = child.wait_with_output();
    match done {
        Ok(out) => (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned()),
        Err(err) => (false, format!("nmcli did not finish: {err}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chonk_netjoin::dialog::clean_reason;

    /// The one property of the argv that matters, asserted where the
    /// argv is written. `join` itself cannot run in a test (it spawns
    /// nmcli), so this pins the shape the function above builds: the
    /// SSID is one element, the passphrase is not an element at all.
    #[test]
    fn the_argv_carries_the_ssid_and_never_the_passphrase() {
        let argv = ["--ask", "device", "wifi", "connect", "--", "Cafe Wifi"];
        assert!(argv.contains(&"--ask"), "without --ask nmcli would want the secret as an argument");
        assert_eq!(argv[argv.len() - 1], "Cafe Wifi", "the ssid is one element, never shell-quoted into one");
        assert!(argv.contains(&"--"), "an ssid may begin with a dash");
        assert!(!argv.iter().any(|a| a.eq_ignore_ascii_case("password")), "the argv has no place to put a secret");
    }

    #[test]
    fn a_failure_reason_survives_the_trip_to_the_dialog() {
        let mut dialog = JoinDialog::new("Cafe");
        dialog.finished(false, "Error: Secrets were required, but not provided.\n");
        let shown = format!("{:?}", dialog.view().phase);
        assert!(shown.contains("SECRETS WERE REQUIRED"), "nmcli's own words reach the screen: {shown}");
        assert_eq!(clean_reason("Error: x"), "X");
    }
}
