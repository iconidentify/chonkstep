//! The token store that lets a dockapp survive the shell's own restart.
//!
//! # What this is for
//!
//! A hot restart of this shell is a `Command::exec` of the on-disk
//! binary — `scripts/restart.sh`, `scripts/update.sh`, and every pick
//! from the theme menu take that path. The process image is replaced, so
//! everything the outgoing shell knew is gone; but a dockapp is not a
//! display-server client, nothing kills it, and the socket path is
//! per-display rather than per-pid precisely so it can knock on the same
//! door again (see `transport::socket_path`). The SDK has retried on EOF
//! for a full ten seconds since Phase 4a.
//!
//! The one thing missing was the credential. A reconnecting dockapp
//! presents the token it was given at launch, and `validate_hello`
//! requires that token to still be the tile's — but the incoming shell
//! minted none of them. Without a handoff the reconnect is refused with
//! `Unauthorized`, which is exactly what should happen to a token no
//! tile issued, and restart survival is impossible by construction.
//!
//! So the outgoing shell writes `id token` for every dockapp it is
//! deliberately leaving running, and the incoming one reads it, deletes
//! it, and holds each named slot open for a few seconds instead of
//! launching a second copy.
//!
//! # The payoff, worth naming
//!
//! With this, **a dockapp survives a theme switch, `scripts/restart.sh`
//! and `scripts/update.sh` — including on the Wayland session, where
//! every ordinary client is killed outright by a compositor restart**
//! (README: "Restart costs you your clients"; there is no Wayland
//! equivalent of the X11 SaveSet). A dock tile that is not a Wayland
//! client therefore gets a guarantee no Wayland client on this desktop
//! can have.
//!
//! # Why a file, and why this file
//!
//! Rejected: passing the tokens in the environment across the `exec`.
//! It works for `restart_in_place` — the environment survives an `exec`
//! — but there are two `restart_in_place` implementations, one per
//! binary, so the mechanism would have to be duplicated and kept in
//! step; and a variable in the shell's own environment is inherited by
//! every application the user then launches from the root menu, which
//! would hand every one of them every dockapp's credential. Removing it
//! again after reading is one forgotten line away from that outcome.
//!
//! Rejected: dropping the token requirement for a reconnect and
//! admitting any registered id. That is an authentication bypass with a
//! ten-second window at every restart, and a permanent one for a tile
//! whose process never came back.
//!
//! A file under `$XDG_RUNTIME_DIR/chonkstep/`, 0600 inside the 0700
//! directory the socket already lives in, is exactly as protected as the
//! socket the token authenticates against, and it also covers the case
//! the environment cannot: a shell that was killed and restarted by the
//! session script rather than re-exec'ing itself.
//!
//! # Honest note on what the token is worth
//!
//! Writing it to a file does not weaken it, because it was never a
//! secret from this user's other processes. The token is handed to the
//! dockapp in `CHONKSTEP_DOCK_TOKEN`, and any process running as you can
//! read `/proc/<pid>/environ` of a process you own. What the token stops
//! is a *stray* process claiming a slot it was not launched for — an
//! accountability boundary, not a defence against a determined local
//! attacker, which the module docs for `crate::dockapp` already say this
//! whole boundary is not.

use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use chonk_dock_proto::transport;
use chonk_dock_proto::TOKEN_BYTES;

/// The handoff file beside a given socket:
/// `$XDG_RUNTIME_DIR/chonkstep/dock-<display>.handoff`.
///
/// Derived from the socket path the listener actually bound rather than
/// recomputed from the display name, so the two cannot disagree. That
/// matters at exactly the moment this feature is used: a Wayland
/// compositor picks its own `WAYLAND_DISPLAY`, and a replacement that
/// picked a different one would bind a different socket — one no
/// surviving dockapp can reach. Keying the handoff off the same string
/// makes "the survivor cannot find the socket" and "the incoming shell
/// finds no tokens" the same condition, so the tiles launch fresh
/// immediately instead of waiting out a rejoin window for a knock that
/// can never come.
///
/// `None` when the socket could not be bound at all, which is a session
/// with no dockapps and therefore nothing to hand over.
pub(crate) fn beside(socket: &Path) -> Option<PathBuf> {
    (socket.extension().is_some()).then(|| socket.with_extension("handoff"))
}

/// Writes the tokens the incoming shell will need.
///
/// Written through a temporary and renamed, so a shell that dies
/// mid-write leaves either the old file or the new one and never half a
/// line. Created 0600 explicitly rather than left to the umask — the
/// directory is already 0700, so this is the second lock, and the same
/// belt-and-braces reasoning `SeqpacketListener::bind` applies to the
/// socket itself.
///
/// A failure here is logged and otherwise ignored: the consequence is
/// dockapps that get relaunched instead of readopted, which is the
/// behaviour of every version before this one.
pub(crate) fn write(path: &Path, tokens: &[(String, [u8; TOKEN_BYTES])]) {
    if tokens.is_empty() {
        // Nothing to hand over, and a stale file from a previous restart
        // would otherwise be read by the incoming shell and hold slots
        // open for processes that are not coming.
        clear(path);
        return;
    }
    let mut body = String::new();
    for (id, token) in tokens {
        // `id` is `is_valid_id`-checked at registry scan time, so it
        // contains no whitespace and no newline and cannot forge a
        // second line here. Asserting it again would be cheap, but the
        // registry is the place that owns the rule.
        body.push_str(id);
        body.push(' ');
        body.push_str(&transport::token_to_hex(token));
        body.push('\n');
    }
    let temporary = path.with_extension("handoff.new");
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)
        .and_then(|mut file| file.write_all(body.as_bytes()))
        .and_then(|()| std::fs::rename(&temporary, path));
    match written {
        Ok(()) => tracing::info!(count = tokens.len(), "handed dockapp tokens to the incoming shell; those tiles keep running"),
        Err(error) => {
            tracing::warn!(?error, path = %path.display(), "could not hand off dockapp tokens; the next shell will relaunch those tiles instead");
            let _ = std::fs::remove_file(&temporary);
        }
    }
}

/// Reads the tokens the outgoing shell left, and deletes the file.
///
/// Deleted on read, always — including when it did not parse. The tokens
/// are in memory from here on, and a file that survived would be
/// re-consumed at the *next* restart, holding slots open for processes
/// that stopped existing two restarts ago.
pub(crate) fn take(path: &Path) -> HashMap<String, [u8; TOKEN_BYTES]> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let _ = std::fs::remove_file(path);
    let mut tokens = HashMap::new();
    for line in text.lines() {
        // Malformed lines are skipped rather than failing the whole
        // file: a torn write should cost one tile its adoption, not
        // every tile.
        let Some((id, hex)) = line.split_once(' ') else { continue };
        let Some(token) = transport::token_from_hex(hex) else { continue };
        if id.is_empty() {
            continue;
        }
        tokens.insert(id.to_string(), token);
    }
    if !tokens.is_empty() {
        tracing::info!(count = tokens.len(), "found dockapp tokens from the previous shell; holding those slots open for a reconnect");
    }
    tokens
}

/// Removes the file if it is there. Used on a session that is genuinely
/// ending, where every dockapp has been told `Goodbye { Shutdown }` and
/// nothing should be adopted next time.
pub(crate) fn clear(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("chonk-handoff-{}-{unique}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn file(&self) -> PathBuf {
            self.0.join("dock-test.handoff")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn token(seed: u8) -> [u8; TOKEN_BYTES] {
        [seed; TOKEN_BYTES]
    }

    #[test]
    fn a_token_survives_the_restart_and_the_file_does_not() {
        let scratch = Scratch::new();
        write(&scratch.file(), &[("clock".into(), token(1)), ("net".into(), token(2))]);

        let taken = take(&scratch.file());
        assert_eq!(taken.get("clock"), Some(&token(1)));
        assert_eq!(taken.get("net"), Some(&token(2)));
        assert!(!scratch.file().exists(), "consumed on read: a file that survived would be re-read at the next restart");
        assert!(take(&scratch.file()).is_empty(), "and a second read finds nothing");
    }

    #[test]
    fn the_file_is_private_to_this_user() {
        // The token is exactly as protected as the socket it
        // authenticates against, and no more: 0600 inside the 0700
        // directory `ensure_socket_dir` already verifies.
        let scratch = Scratch::new();
        write(&scratch.file(), &[("clock".into(), token(1))]);
        let mode = std::fs::metadata(scratch.file()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a credential file must not be readable by group or other");
    }

    #[test]
    fn nothing_to_hand_off_removes_a_stale_file_rather_than_leaving_it() {
        let scratch = Scratch::new();
        write(&scratch.file(), &[("clock".into(), token(1))]);
        write(&scratch.file(), &[]);
        assert!(take(&scratch.file()).is_empty(), "a shell with no dockapps must not resurrect the last shell's list");
    }

    #[test]
    fn a_torn_line_costs_one_tile_its_adoption_and_not_the_file() {
        let scratch = Scratch::new();
        std::fs::write(
            scratch.file(),
            format!("clock {}\nnet not-hex\nno-space-here\n\nsound {}\n", transport::token_to_hex(&token(1)), transport::token_to_hex(&token(2))),
        )
        .unwrap();
        let taken = take(&scratch.file());
        assert_eq!(taken.len(), 2, "the readable lines are read: {taken:?}");
        assert_eq!(taken.get("clock"), Some(&token(1)));
        assert_eq!(taken.get("sound"), Some(&token(2)));
    }

    #[test]
    fn the_handoff_lives_beside_the_socket_it_authenticates() {
        // Same directory, same per-display key, derived from the socket
        // rather than recomputed. A handoff under a different name than
        // the socket would be a token offered to a session that could not
        // be reached anyway.
        let socket = PathBuf::from("/run/user/1000/chonkstep/dock-wayland-1.sock");
        let handoff = beside(&socket).expect("a bound socket has a handoff beside it");
        assert_eq!(handoff.parent(), socket.parent());
        assert_eq!(handoff.file_name().unwrap(), "dock-wayland-1.handoff");
        assert_eq!(beside(Path::new("")), None, "a session that could not bind has nothing to hand over");
    }
}
