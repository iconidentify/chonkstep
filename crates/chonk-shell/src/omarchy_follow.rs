//! Noticing that Omarchy changed its theme — or its background — so a
//! session that follows it changes too, with no hook installed on
//! Omarchy's side.
//!
//! `omarchy-theme-set` swaps `current/theme` atomically (`rm -rf` the
//! old copy, `mv` the staged new one into place) and *then* writes
//! `current/theme.name`. That ordering is the contract this module
//! rides: when `theme.name` is seen to change, `colors.toml` under it
//! is already the new theme's, so a single re-resolve reads a
//! consistent palette. The palette file's own identity (its mtime and
//! size) is folded into the signature too, for the less tidy cases —
//! a theme edited in place, a theme *appearing* after a session
//! started following an empty state directory, or Omarchy being
//! uninstalled under a running desk.
//!
//! The background is the third ingredient. `omarchy-theme-set` and
//! `omarchy-theme-bg-set` both end with `ln -nsf <image>
//! current/background`, so the link's *target* is the background's
//! identity, and it is read (not followed) into the signature: a
//! cycle to the next picture changes the target and nothing else. The
//! target file's own mtime and size ride along for a picture edited
//! in place under the same name. Omarchy's own shell polls this very
//! link for the same reason.
//!
//! Polled at one hertz from `Shell::tick`, not watched with inotify:
//! the same argument the reload marker and the dockapp theme
//! broadcast make (`startup::reload_requested`), plus a specific one —
//! the directory is *replaced*, not modified, and an inotify watch on
//! a path that is unlinked and recreated has to be re-armed by exactly
//! the kind of code that goes wrong at 3 a.m. Two `stat` calls a
//! second on paths that are usually there is nothing, and a theme
//! change landing within a second of the user pressing the key is
//! indistinguishable from instant beside Omarchy's own reload fan-out.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// How often `Shell::tick` looks. A second is the cadence the brief
/// asked for and comfortably below what a person can notice after an
/// Omarchy theme switch, which itself takes a visible moment to fan
/// out to every application.
const CADENCE: Duration = Duration::from_secs(1);

/// The identity of Omarchy's current look on disk, cheap to take and
/// compared by equality: `theme.name`'s mtime, `colors.toml`'s
/// (mtime, size), and the `background` link's target with the target's
/// (mtime, size) — each `None` when the file is not there.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Signature {
    name: Option<SystemTime>,
    colors: Option<(SystemTime, u64)>,
    background: Option<(PathBuf, Option<(SystemTime, u64)>)>,
}

impl Signature {
    fn of(current: &Path) -> Self {
        let name = std::fs::metadata(current.join("theme.name")).and_then(|m| m.modified()).ok();
        let colors = Self::identity(&current.join("theme/colors.toml"));
        let link = current.join("background");
        // `read_link`, not `metadata`: the link is what Omarchy moves.
        // A background that is a plain file rather than a link (a
        // hand-made state directory) is identified by its own path, so
        // its contents' identity below is still what changes.
        let background = std::fs::symlink_metadata(&link).ok().map(|meta| {
            let target = if meta.file_type().is_symlink() { std::fs::read_link(&link).unwrap_or(link.clone()) } else { link.clone() };
            let identity = Self::identity(&link);
            (target, identity)
        });
        Self { name, colors, background }
    }

    /// (mtime, size) of the file at `path`, through any link.
    fn identity(path: &Path) -> Option<(SystemTime, u64)> {
        std::fs::metadata(path).ok().and_then(|m| Some((m.modified().ok()?, m.len())))
    }
}

/// Watches Omarchy's `current` directory for a theme change. Owned by
/// the shell whether or not the session follows Omarchy — the shell
/// only *asks* it while following, so on a session dressed in a
/// built-in it costs a struct in memory and nothing else.
#[derive(Debug)]
pub struct Watch {
    last_checked: Option<Instant>,
    seen: Option<Signature>,
}

impl Default for Watch {
    fn default() -> Self {
        Self::new()
    }
}

impl Watch {
    pub fn new() -> Self {
        Self { last_checked: None, seen: None }
    }

    /// Whether Omarchy's current theme or background has changed since
    /// the last time this returned `true` — or since the watch was
    /// created, for its first look. Rate-limited to [`CADENCE`]: calls inside the window
    /// return `false` without touching the disk.
    ///
    /// The first call after construction baselines and returns `false`:
    /// the session just resolved its look from the very files being
    /// watched, so what is there now is what it is wearing. The
    /// baseline is never reset afterwards, on purpose: a reload or a
    /// theme pick that re-resolves in between simply means the next
    /// difference this sees triggers one redundant resolve, which
    /// `Shell::apply_session_state` recognises as a no-op — whereas
    /// resetting would open a window (between that resolve and the
    /// next look) in which a real change could be baselined away.
    pub fn changed(&mut self, now: Instant) -> bool {
        let Some(current) = wm_theme::omarchy::current_dir() else {
            return false;
        };
        self.changed_in(&current, now)
    }

    /// [`Self::changed`] against an explicit directory, for tests.
    pub fn changed_in(&mut self, current: &Path, now: Instant) -> bool {
        if self.last_checked.is_some_and(|last| now.duration_since(last) < CADENCE) {
            return false;
        }
        self.last_checked = Some(now);
        let signature = Signature::of(current);
        let changed = self.seen.as_ref().is_some_and(|seen| *seen != signature);
        self.seen = Some(signature);
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("chonk-omarchy-follow-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("theme")).unwrap();
        dir
    }

    /// A different mtime for sure, without sleeping past the
    /// filesystem's timestamp granularity: set it explicitly.
    fn touch(path: &Path, seconds_ago: u64) {
        let when = SystemTime::now() - Duration::from_secs(seconds_ago);
        std::fs::File::options()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    #[test]
    fn the_first_look_baselines_and_only_a_later_difference_counts() {
        let dir = scratch("baseline");
        std::fs::write(dir.join("theme/colors.toml"), "a").unwrap();
        touch(&dir.join("theme.name"), 100);
        let mut watch = Watch::new();
        let t0 = Instant::now();
        assert!(!watch.changed_in(&dir, t0), "first look is a baseline, not a change");
        assert!(!watch.changed_in(&dir, t0 + Duration::from_secs(2)), "nothing moved");
        touch(&dir.join("theme.name"), 10);
        assert!(watch.changed_in(&dir, t0 + Duration::from_secs(4)), "theme.name rewritten");
        assert!(!watch.changed_in(&dir, t0 + Duration::from_secs(6)), "reported once");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn looks_inside_the_cadence_window_do_not_touch_the_disk() {
        let dir = scratch("cadence");
        touch(&dir.join("theme.name"), 100);
        let mut watch = Watch::new();
        let t0 = Instant::now();
        assert!(!watch.changed_in(&dir, t0));
        touch(&dir.join("theme.name"), 10);
        assert!(!watch.changed_in(&dir, t0 + Duration::from_millis(500)), "too soon to look");
        assert!(watch.changed_in(&dir, t0 + Duration::from_millis(1000)), "the second look sees it");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `omarchy-theme-bg-next`: the link is repointed at another file
    /// and nothing under `theme/` moves. That alone must count — and
    /// so must the picture behind an unmoved link being rewritten.
    #[test]
    fn repointing_the_background_link_is_a_change() {
        let dir = scratch("background");
        std::fs::create_dir_all(dir.join("theme/backgrounds")).unwrap();
        let first = dir.join("theme/backgrounds/1-first.webp");
        let second = dir.join("theme/backgrounds/2-second.webp");
        std::fs::write(&first, "one").unwrap();
        std::fs::write(&second, "two").unwrap();
        let link = dir.join("background");
        std::os::unix::fs::symlink(&first, &link).unwrap();

        let mut watch = Watch::new();
        let t0 = Instant::now();
        assert!(!watch.changed_in(&dir, t0), "baseline");
        assert!(!watch.changed_in(&dir, t0 + Duration::from_secs(2)), "nothing moved");

        // `ln -nsf second background`: the link's target moves, and
        // its own mtime is not consulted, so replacing it in place is
        // seen for the target and not the timestamp.
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&second, &link).unwrap();
        assert!(watch.changed_in(&dir, t0 + Duration::from_secs(4)), "the link points somewhere else");
        assert!(!watch.changed_in(&dir, t0 + Duration::from_secs(6)), "reported once");

        // The same target, rewritten: a picture edited under its name.
        std::fs::write(&second, "two, retouched").unwrap();
        assert!(watch.changed_in(&dir, t0 + Duration::from_secs(8)), "the file behind the link changed");

        // The link removed altogether — no background — is a change too.
        std::fs::remove_file(&link).unwrap();
        assert!(watch.changed_in(&dir, t0 + Duration::from_secs(10)), "vanished");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_palette_appearing_or_vanishing_or_growing_is_a_change() {
        let dir = scratch("colors");
        let mut watch = Watch::new();
        let t0 = Instant::now();
        assert!(!watch.changed_in(&dir, t0), "empty state dir baselines too");
        std::fs::write(dir.join("theme/colors.toml"), "a").unwrap();
        assert!(watch.changed_in(&dir, t0 + Duration::from_secs(2)), "appeared");
        std::fs::write(dir.join("theme/colors.toml"), "longer").unwrap();
        assert!(watch.changed_in(&dir, t0 + Duration::from_secs(4)), "grew");
        std::fs::remove_file(dir.join("theme/colors.toml")).unwrap();
        assert!(watch.changed_in(&dir, t0 + Duration::from_secs(6)), "vanished");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
