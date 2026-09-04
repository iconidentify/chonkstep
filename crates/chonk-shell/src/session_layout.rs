//! Session layout persistence and restore — the half of the "living
//! desktop" the shell owns. While a session runs, the shell records
//! every managed window's application identity, content geometry,
//! workspace and shape flags into one state file; at the next opt-in
//! startup (`restore_session = true`) it relaunches those applications
//! and, as their windows map, puts each one back where its record says.
//!
//! Backend-generic by construction, like everything in this crate: the
//! store speaks in `WindowRecord`s the shell distills from
//! `wm_core::Client`s, so the X11 session and the Wayland compositor
//! restore identically because they run this very code.
//!
//! # The rules that shape the design
//!
//! - **A window the user closed is forgotten.** The store never edits
//!   records in place; every persist rewrites the file from a snapshot
//!   of the *live* client set, so a closed window drops out of the next
//!   snapshot and therefore out of the file by construction. There is
//!   no "remove on Destroyed" code to forget to call.
//! - **Persist on settle, not on motion.** A drag produces geometry
//!   changes at input rate; writing the file on each would be the
//!   launchdock's persist-on-commit idiom inverted into a disk grinder.
//!   Instead the shell hands the store a snapshot once per tick, and
//!   the store writes only once the snapshot has held still for
//!   [`DEBOUNCE`] and differs from what is on disk.
//! - **A crash mid-write must never eat the layout.** The file is
//!   written to a sibling temp path and renamed over the original —
//!   rename is atomic on the same filesystem, so the file is always
//!   either the old complete layout or the new complete one.
//! - **Matching is first-come-first-matched per class.** When a
//!   relaunched application maps, the first pending record with its
//!   window class claims it — the pragmatic rule every session-restore
//!   implementation lands on, because nothing sturdier exists: a
//!   relaunched process shares no identity with its predecessor beyond
//!   its class.
//! - **A record whose application never maps expires quietly.** After
//!   [`RESTORE_GRACE`] the pending list is dropped with a log line;
//!   an uninstalled app or a launch that failed must not leave the
//!   shell holding stale records forever — nor, worse, applying one to
//!   some unrelated window mapped an hour later.
//!
//! While a restore is still pending, persistence is suppressed: the
//! moment after startup the live client set is empty, and writing that
//! snapshot would wipe the very file being restored — so a crash
//! during the restore window would lose the layout. Recording resumes
//! when every pending record is matched or expired.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use wm_theme_api::{Point, Rect, Size};

use crate::apps::{match_window_class, AppEntry};

/// How long a changed layout must hold still before it is written.
/// Long enough that a drag writes once at its end rather than along
/// its path, short enough that the window between "the user arranged
/// something" and "a crash would keep it" stays negligible.
pub const DEBOUNCE: Duration = Duration::from_secs(2);

/// How long after startup a pending record may wait for its window.
/// Generous because a cold application launch on a busy disk is slow;
/// bounded because a record that outlived its grace would otherwise
/// claim whatever same-class window the user opens next week.
pub const RESTORE_GRACE: Duration = Duration::from_secs(30);

/// One remembered window: everything restore needs to relaunch its
/// application and re-place its window, and nothing more.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowRecord {
    /// The window's `WM_CLASS` class / Wayland app id — the matching
    /// key at restore time.
    pub class: String,
    /// The `.desktop` id the class resolved to when the record was
    /// made (`None` when nothing matched). Resolved at record time
    /// rather than restore time so a layout survives the application
    /// index changing shape between sessions — the id is looked up
    /// again at restore and falls back to a fresh class match.
    pub app: Option<String>,
    /// Root-relative *content* geometry. For a maximized window this
    /// is the pre-maximize geometry (the one worth restoring — the
    /// maximize itself is re-derived from the flag against whatever
    /// workarea the new session has).
    pub geometry: Rect,
    pub workspace: usize,
    pub maximized: bool,
    pub shaded: bool,
    pub miniaturized: bool,
}

/// How one record's application should be brought back. Split from the
/// spawning itself so this module stays free of theme/spawn plumbing
/// (and therefore trivially testable): the shell owns the actual
/// launches, through the very paths its menus use.
#[derive(Clone, Debug, PartialEq)]
pub enum RelaunchPlan {
    /// The shell's own themed terminal — special-cased by class
    /// because launching foot through its bare `.desktop` entry would
    /// lose the theme's palette, font and geometry that
    /// `spawn_terminal` exists to apply.
    Terminal,
    /// An ordinary `.desktop` application, through the shell's launch
    /// fixups (scale env, platform args) like any menu launch.
    App(AppEntry),
}

/// The store: what has been recorded, what is waiting to be restored,
/// and when the file was last worth writing.
pub struct SessionLayout {
    path: Option<PathBuf>,
    /// Records loaded at startup and not yet claimed by a mapped
    /// window. Non-empty exactly while a restore is in progress.
    pending: Vec<WindowRecord>,
    /// When the pending records stop waiting — set once at startup.
    restore_deadline: Option<Instant>,
    /// The most recent snapshot the shell handed over.
    last_snapshot: Vec<WindowRecord>,
    /// When `last_snapshot` last changed — the debounce clock.
    settled_at: Instant,
    /// What the file on disk holds, as far as this process knows.
    /// `None` until the first write (or load) tells us.
    persisted: Option<Vec<WindowRecord>>,
}

impl SessionLayout {
    /// Opens the store against the shared state directory. When
    /// `restore` is set, loads the previous session's records and
    /// returns the launch plans for them, in file order; otherwise the
    /// previous layout is simply the base the first persist overwrites.
    pub fn start(restore: bool, apps: &[AppEntry], now: Instant) -> (Self, Vec<RelaunchPlan>) {
        Self::start_at(crate::startup::session_layout_path().into(), restore, apps, now)
    }

    /// The path-injected core of [`Self::start`], for tests that must
    /// not touch the real state directory.
    pub fn start_at(path: Option<PathBuf>, restore: bool, apps: &[AppEntry], now: Instant) -> (Self, Vec<RelaunchPlan>) {
        let mut layout = Self {
            path,
            pending: Vec::new(),
            restore_deadline: None,
            last_snapshot: Vec::new(),
            settled_at: now,
            persisted: None,
        };
        if !restore {
            return (layout, Vec::new());
        }
        let Some(text) = layout.path.as_deref().and_then(|p| std::fs::read_to_string(p).ok()) else {
            // First opt-in session, or nothing recorded yet: nothing
            // to restore is the normal case, not a problem.
            return (layout, Vec::new());
        };
        let records = parse(&text);
        let plans: Vec<RelaunchPlan> = records.iter().filter_map(|record| relaunch_plan(record, apps)).collect();
        tracing::info!(
            windows = records.len(),
            launching = plans.len(),
            "restoring the previous session's layout"
        );
        layout.persisted = Some(records.clone());
        layout.pending = records;
        layout.restore_deadline = (!layout.pending.is_empty()).then(|| now + RESTORE_GRACE);
        (layout, plans)
    }

    /// One housekeeping pass: expire an overdue restore, then decide
    /// whether the snapshot has settled into something worth writing —
    /// and write it if so. Call once per shell tick with a snapshot of
    /// the live client set.
    pub fn service(&mut self, snapshot: Vec<WindowRecord>, now: Instant) {
        if self.note(snapshot, now) {
            self.write_out();
        }
    }

    /// Advances expiry and debounce timers when the caller has proved
    /// that the live arrangement still equals [`Self::current`].
    ///
    /// Keeping this separate from [`Self::service`] matters on the
    /// compositor's steady-state path: constructing a snapshot clones
    /// every window class and resolves every class through the desktop
    /// application index. None of that work contributes anything when
    /// the arrangement has not moved, but the debounce clock still has
    /// to mature so the last real change reaches disk. This entry point
    /// performs exactly that timer half without manufacturing an
    /// identical `Vec<WindowRecord>` first.
    pub fn service_current(&mut self, now: Instant) {
        if self.ready(now) {
            self.write_out();
        }
    }

    /// The most recent live arrangement handed to [`Self::service`].
    /// Callers may compare borrowed client state against it and use
    /// [`Self::service_current`] when it is still exact.
    pub fn current(&self) -> &[WindowRecord] {
        &self.last_snapshot
    }

    /// The pure decision core of [`Self::service`] — updates the
    /// debounce/expiry state and answers "write now?". Split out so
    /// the rules are testable with plain `Instant` arithmetic and no
    /// filesystem. When it returns `true` the store already considers
    /// `last_snapshot` persisted; the caller's job is only the I/O.
    fn note(&mut self, snapshot: Vec<WindowRecord>, now: Instant) -> bool {
        if snapshot != self.last_snapshot {
            self.last_snapshot = snapshot;
            self.settled_at = now;
        }
        self.ready(now)
    }

    /// Advances the time-dependent half of [`Self::note`] against the
    /// snapshot already held in `last_snapshot`.
    fn ready(&mut self, now: Instant) -> bool {
        if let Some(deadline) = self.restore_deadline {
            if !self.pending.is_empty() && now >= deadline {
                tracing::info!(
                    unmatched = self.pending.len(),
                    "session restore grace elapsed; letting the unmatched records go"
                );
                self.pending.clear();
            }
            if self.pending.is_empty() {
                self.restore_deadline = None;
            }
        }
        // Suppressed mid-restore: the live set is still filling in,
        // and writing it now would replace the full recorded layout
        // with a partial one — see the module doc.
        if !self.pending.is_empty() {
            return false;
        }
        if self.persisted.as_ref() == Some(&self.last_snapshot) {
            return false;
        }
        if now < self.settled_at + DEBOUNCE {
            return false;
        }
        self.persisted = Some(self.last_snapshot.clone());
        true
    }

    /// Claims the first pending record whose class matches a freshly
    /// mapped window's — the first-come-first-matched rule. `None`
    /// once the restore is over (or was never on), which is the
    /// permanent steady state; callers need no separate "is a restore
    /// running" check.
    pub fn claim(&mut self, class: &str, now: Instant) -> Option<WindowRecord> {
        if self.restore_deadline.is_some_and(|deadline| now >= deadline) {
            return None;
        }
        let index = self.pending.iter().position(|record| record.class.eq_ignore_ascii_case(class))?;
        Some(self.pending.remove(index))
    }

    /// Writes the current snapshot out regardless of the debounce —
    /// the shutdown path, so a window closed moments before logout is
    /// forgotten rather than resurrected next login. Skipped while a
    /// restore is still pending, for the same partial-layout reason
    /// `note` suppresses ordinary persists then.
    pub fn flush(&mut self) {
        if !self.pending.is_empty() || self.persisted.as_ref() == Some(&self.last_snapshot) {
            return;
        }
        self.persisted = Some(self.last_snapshot.clone());
        self.write_out();
    }

    /// The I/O half of a persist: serialize `last_snapshot` and write
    /// it atomically. A failure warns and leaves the previous file
    /// intact — the next settled change tries again.
    fn write_out(&mut self) {
        let Some(path) = self.path.as_deref() else {
            return;
        };
        if let Err(error) = write_atomic(path, &serialize(&self.last_snapshot)) {
            tracing::warn!(?error, path = %path.display(), "could not persist the session layout");
            // Disk truth is now unknown; clearing the cache makes the
            // next settle retry rather than believe this write landed.
            self.persisted = None;
        }
    }
}

/// The launch plan for one record, against the current application
/// index: the shell's own terminal by class, else the recorded
/// `.desktop` id, else a fresh class match. `None` — an application
/// no longer installed, or a window that never resolved to one — means
/// nothing is launched and the record waits out its grace in case the
/// window arrives some other way.
fn relaunch_plan(record: &WindowRecord, apps: &[AppEntry]) -> Option<RelaunchPlan> {
    if record.class.eq_ignore_ascii_case("foot") {
        return Some(RelaunchPlan::Terminal);
    }
    if let Some(id) = &record.app {
        if let Some(entry) = apps.iter().find(|app| &app.id == id) {
            return Some(RelaunchPlan::App(entry.clone()));
        }
    }
    match_window_class(apps, &record.class).map(|index| RelaunchPlan::App(apps[index].clone()))
}

// -- the wire format -----------------------------------------------------

/// One tab-separated record per line, human-inspectable like the
/// theme/wallpaper/dock files beside it:
///
/// ```text
/// class \t app-or-'-' \t x \t y \t w \t h \t workspace \t flags-or-'-'
/// ```
///
/// Tabs because a class is free text that may contain spaces; flags
/// are a comma-joined subset of `maximized,shaded,miniaturized`. A
/// line that does not parse is skipped with a warning — one corrupted
/// record must cost that record, never the layout.
fn serialize(records: &[WindowRecord]) -> String {
    let mut text = String::new();
    for record in records {
        let mut flags: Vec<&str> = Vec::new();
        if record.maximized {
            flags.push("maximized");
        }
        if record.shaded {
            flags.push("shaded");
        }
        if record.miniaturized {
            flags.push("miniaturized");
        }
        let flags = if flags.is_empty() { "-".to_string() } else { flags.join(",") };
        text.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            record.class,
            record.app.as_deref().unwrap_or("-"),
            record.geometry.pos.x,
            record.geometry.pos.y,
            record.geometry.size.w,
            record.geometry.size.h,
            record.workspace,
            flags,
        ));
    }
    text
}

fn parse(text: &str) -> Vec<WindowRecord> {
    text.lines().filter(|line| !line.trim().is_empty()).filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<WindowRecord> {
    let fields: Vec<&str> = line.split('\t').collect();
    let parsed = (|| -> Option<WindowRecord> {
        let [class, app, x, y, w, h, workspace, flags] = fields.as_slice() else {
            return None;
        };
        if class.is_empty() {
            return None;
        }
        // Zero-sized windows can't have been recorded by `snapshot`;
        // a record claiming one is corruption, and restoring it would
        // hand the client a degenerate configure.
        let (w, h) = (w.parse::<u32>().ok()?, h.parse::<u32>().ok()?);
        if w == 0 || h == 0 {
            return None;
        }
        Some(WindowRecord {
            class: class.to_string(),
            app: (*app != "-" && !app.is_empty()).then(|| app.to_string()),
            geometry: Rect {
                pos: Point::new(x.parse().ok()?, y.parse().ok()?),
                size: Size::new(w, h),
            },
            workspace: workspace.parse().ok()?,
            maximized: flags.split(',').any(|f| f == "maximized"),
            shaded: flags.split(',').any(|f| f == "shaded"),
            miniaturized: flags.split(',').any(|f| f == "miniaturized"),
        })
    })();
    if parsed.is_none() {
        tracing::warn!(line, "skipping an unparsable session-layout record");
    }
    parsed
}

/// Temp-and-rename write: the layout file is always either the old
/// complete layout or the new complete one, never a torn write — the
/// whole point of persisting is surviving a crash, and a crash is
/// allowed to happen mid-write.
fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::AppCategory;

    fn record(class: &str, x: i32) -> WindowRecord {
        WindowRecord {
            class: class.to_string(),
            app: None,
            geometry: Rect { pos: Point::new(x, 40), size: Size::new(500, 400) },
            workspace: 0,
            maximized: false,
            shaded: false,
            miniaturized: false,
        }
    }

    fn entry(id: &str) -> AppEntry {
        AppEntry {
            id: id.to_string(),
            name: id.to_string(),
            exec: vec![id.to_string()],
            terminal: false,
            category: AppCategory::Other,
            startup_wm_class: None,
        }
    }

    fn fresh(now: Instant) -> SessionLayout {
        SessionLayout::start_at(None, false, &[], now).0
    }

    #[test]
    fn the_wire_format_round_trips_every_field() {
        let records = vec![
            WindowRecord {
                class: "Navigator".to_string(),
                app: Some("org.mozilla.firefox".to_string()),
                geometry: Rect { pos: Point::new(-40, 12), size: Size::new(1280, 900) },
                workspace: 2,
                maximized: true,
                shaded: false,
                miniaturized: true,
            },
            record("foot", 100),
        ];
        assert_eq!(parse(&serialize(&records)), records);
    }

    #[test]
    fn a_corrupt_line_costs_that_record_and_nothing_else() {
        let good = record("foot", 100);
        let mut text = serialize(std::slice::from_ref(&good));
        text.push_str("this line is not a record\n");
        text.push_str("also\tbad\t1\t2\t3\n");
        // A zero-sized geometry is corruption, not a window.
        text.push_str("gimp\t-\t0\t0\t0\t0\t0\t-\n");
        text.push_str(&serialize(&[record("gimp", 300)]));
        assert_eq!(parse(&text), vec![good, record("gimp", 300)]);
    }

    #[test]
    fn a_settled_change_persists_after_the_debounce_and_not_before() {
        let start = Instant::now();
        let mut layout = fresh(start);
        // The change arrives...
        assert!(!layout.note(vec![record("foot", 100)], start), "nothing persists on arrival");
        // ...keeps not persisting while inside the debounce...
        assert!(!layout.note(vec![record("foot", 100)], start + DEBOUNCE / 2));
        // ...and persists once it has held still long enough.
        assert!(layout.note(vec![record("foot", 100)], start + DEBOUNCE));
        // Persisted state is remembered: the same snapshot never
        // writes twice.
        assert!(!layout.note(vec![record("foot", 100)], start + DEBOUNCE * 2));
    }

    #[test]
    fn an_unchanged_borrowed_snapshot_still_matures_the_debounce() {
        let start = Instant::now();
        let mut layout = fresh(start);
        let snapshot = vec![record("foot", 100)];
        layout.service(snapshot.clone(), start);

        assert_eq!(layout.current(), snapshot);
        layout.service_current(start + DEBOUNCE / 2);
        assert!(layout.persisted.is_none(), "an unchanged arrangement is still inside its debounce");

        layout.service_current(start + DEBOUNCE);
        assert_eq!(layout.persisted.as_deref(), Some(snapshot.as_slice()));
        layout.service_current(start + DEBOUNCE * 2);
        assert_eq!(layout.persisted.as_deref(), Some(snapshot.as_slice()), "it is not re-persisted on every tick");
    }

    #[test]
    fn motion_keeps_resetting_the_debounce_clock() {
        // The disk-grinder case: a drag emits a different snapshot
        // every tick, and none of them may hit the disk until the
        // window holds still.
        let start = Instant::now();
        let mut layout = fresh(start);
        for tick in 0..200 {
            let now = start + Duration::from_millis(16 * tick);
            assert!(!layout.note(vec![record("foot", tick as i32)], now), "mid-drag tick {tick} must not persist");
        }
        let released = start + Duration::from_millis(16 * 200);
        assert!(layout.note(vec![record("foot", 199)], released + DEBOUNCE), "the drag's end persists once, settled");
    }

    #[test]
    fn a_closed_window_vanishes_from_the_next_persist() {
        // "Restore must never resurrect what was deliberately
        // dismissed": closing is observable as the window dropping out
        // of the snapshot, and the file is always rewritten whole.
        let start = Instant::now();
        let mut layout = fresh(start);
        // Two windows arrive and settle...
        layout.note(vec![record("foot", 100), record("gimp", 300)], start);
        assert!(layout.note(vec![record("foot", 100), record("gimp", 300)], start + DEBOUNCE));
        // ...one closes (drops out of the snapshot), and the next
        // settled persist no longer contains it.
        layout.note(vec![record("gimp", 300)], start + DEBOUNCE * 2);
        assert!(layout.note(vec![record("gimp", 300)], start + DEBOUNCE * 3));
        assert_eq!(layout.persisted.as_deref(), Some(&[record("gimp", 300)][..]));
    }

    #[test]
    fn claiming_is_first_come_first_matched_per_class_and_case_insensitive() {
        let now = Instant::now();
        let (mut layout, _) = SessionLayout::start_at(None, false, &[], now);
        layout.pending = vec![record("foot", 100), record("foot", 300), record("gimp", 500)];
        layout.restore_deadline = Some(now + RESTORE_GRACE);

        assert_eq!(layout.claim("Foot", now), Some(record("foot", 100)), "first record for the class, case-insensitively");
        assert_eq!(layout.claim("foot", now), Some(record("foot", 300)), "second mapping takes the second record");
        assert_eq!(layout.claim("foot", now), None, "no third record to claim");
        assert_eq!(layout.claim("emacs", now), None, "a window with no record follows normal placement");
        assert_eq!(layout.claim("gimp", now), Some(record("gimp", 500)));
    }

    #[test]
    fn unmatched_records_expire_at_the_grace_deadline() {
        let now = Instant::now();
        let (mut layout, _) = SessionLayout::start_at(None, false, &[], now);
        layout.pending = vec![record("gimp", 500)];
        layout.restore_deadline = Some(now + RESTORE_GRACE);

        // Past the deadline, nothing claims and the pending list goes —
        // and the live truth (an empty desktop, settled since startup)
        // is free to persist over the stale records.
        assert_eq!(layout.claim("gimp", now + RESTORE_GRACE), None, "an expired record must not claim a late window");
        assert!(layout.note(Vec::new(), now + RESTORE_GRACE));
        assert!(layout.pending.is_empty(), "expiry lets the records go");
        // With the restore over, recording works normally again.
        layout.note(vec![record("foot", 100)], now + RESTORE_GRACE + DEBOUNCE);
        assert!(layout.note(vec![record("foot", 100)], now + RESTORE_GRACE + DEBOUNCE * 2));
    }

    #[test]
    fn persistence_is_suppressed_while_a_restore_is_pending() {
        // The moment after startup the live set is empty; writing that
        // would wipe the very layout being restored, so a crash during
        // the restore would lose it.
        let now = Instant::now();
        let (mut layout, _) = SessionLayout::start_at(None, false, &[], now);
        layout.pending = vec![record("foot", 100)];
        layout.restore_deadline = Some(now + RESTORE_GRACE);

        assert!(!layout.note(Vec::new(), now + DEBOUNCE * 2), "an empty snapshot mid-restore must not persist");
        let claimed = layout.claim("foot", now + DEBOUNCE * 2);
        assert!(claimed.is_some());
        // Restore complete: the live set persists again (once settled).
        layout.note(vec![record("foot", 100)], now + DEBOUNCE * 3);
        assert!(layout.note(vec![record("foot", 100)], now + DEBOUNCE * 4));
    }

    #[test]
    fn start_reads_the_file_and_plans_the_relaunches() {
        let dir = std::env::temp_dir().join(format!("chonk-session-layout-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session");
        let mut firefox = record("Navigator", 100);
        firefox.app = Some("org.mozilla.firefox".to_string());
        let records = vec![firefox, record("foot", 300), record("uninstalled-thing", 500)];
        write_atomic(&path, &serialize(&records)).unwrap();

        let apps = [entry("org.mozilla.firefox")];
        let now = Instant::now();
        let (mut layout, plans) = SessionLayout::start_at(Some(path.clone()), true, &apps, now);

        // Firefox by its recorded .desktop id, the terminal by its
        // class special-case; the uninstalled app launches nothing but
        // its record still waits out the grace.
        assert_eq!(plans, vec![RelaunchPlan::App(apps[0].clone()), RelaunchPlan::Terminal]);
        assert_eq!(layout.pending.len(), 3);
        assert!(layout.claim("navigator", now).is_some());

        // And with restore off, the same file loads nothing.
        let (layout_off, plans_off) = SessionLayout::start_at(Some(path), false, &apps, now);
        assert!(plans_off.is_empty());
        assert!(layout_off.pending.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
        drop(layout_off);
        let _ = layout.claim("foot", now);
    }

    #[test]
    fn service_writes_the_file_atomically_and_flush_skips_a_pending_restore() {
        let dir = std::env::temp_dir().join(format!("chonk-session-layout-io-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("session");
        let now = Instant::now();
        let (mut layout, _) = SessionLayout::start_at(Some(path.clone()), false, &[], now);

        layout.service(vec![record("foot", 100)], now);
        assert!(!path.exists(), "nothing on disk before the debounce");
        layout.service(vec![record("foot", 100)], now + DEBOUNCE);
        assert_eq!(parse(&std::fs::read_to_string(&path).unwrap()), vec![record("foot", 100)]);
        assert!(!path.with_extension("tmp").exists(), "the temp file is renamed away, not left behind");

        // A pending restore blocks flush — the file keeps the full
        // recorded layout, not a partial live set.
        layout.pending = vec![record("gimp", 300)];
        layout.last_snapshot = Vec::new();
        layout.flush();
        assert_eq!(parse(&std::fs::read_to_string(&path).unwrap()), vec![record("foot", 100)]);

        // With the restore done, flush writes immediately — no debounce
        // on the way out of the process.
        layout.pending.clear();
        layout.last_snapshot = vec![record("foot", 700)];
        layout.flush();
        assert_eq!(parse(&std::fs::read_to_string(&path).unwrap()), vec![record("foot", 700)]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
