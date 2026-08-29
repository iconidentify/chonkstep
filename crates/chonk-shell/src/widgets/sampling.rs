//! Declarative sampling: the half of the widget SDK that exists so a
//! dock widget cannot reach a syscall.
//!
//! # The rule this module enforces
//!
//! A widget says *what* it needs — this command, that file, this sysfs
//! subtree, at this interval — and never *when* or *on which thread*.
//! The dock owns every sampler thread; [`DockWidget::update`] is handed
//! a [`Samples`] of readings that have already been collected and is a
//! pure fold over them. A widget never gets to say "now", so there is
//! nothing left that a `read_to_string`, a `read_dir` or a
//! `Command::output` written inside a widget would buy it. (Making one
//! written anyway a *compile* error is a per-crate `clippy.toml` in the
//! next phase; see `super`'s module docs. This module is what makes
//! that lint cost nothing to obey.)
//!
//! [`DockWidget::update`]: super::DockWidget::update
//!
//! # Why it is shaped this way rather than "just be careful"
//!
//! On 2026-08-29 the wifi tile sampled the system by calling
//! `nmcli dev wifi` inline from `tick()`. `tick()` runs on the
//! compositor's single repaint thread and `nmcli dev wifi` defaults to
//! `--rescan auto`, which blocks for a full hardware scan whenever
//! NetworkManager's cache is older than thirty seconds: ~3.6s at a
//! time, once every ~34s, during which the desktop drew nothing, read
//! no input, and did not collect the page-flip completion already
//! sitting in its DRM fd. The compositor's own stall watchdog then
//! reported a display-driver fault. Four agents found the wifi icon.
//!
//! The lesson taken was not "review sampling code harder". A
//! synchronous read looks identical wherever it is written; the entire
//! difference is which thread reaches it, and that is not visible in
//! the line. So the trait no longer offers a moment at which a widget
//! could write one. [`super::SupervisedWidget`] still times every call
//! — it covers what this cannot (a quadratic render, a pathological
//! text-shaping path, a future out-of-process tile) — but it notices a
//! freeze afterwards, where this makes the freeze structurally
//! unavailable.
//!
//! # What moved onto worker threads
//!
//! Every one of these was, until this module landed, executed on the
//! repaint thread:
//!
//! * `/proc/stat`, `/proc/meminfo` (sysload) — [`Source::File`]
//! * `/proc/net/dev` (net) — [`Source::File`]
//! * `read_dir` + up to four reads per supply over
//!   `/sys/class/power_supply` (power) — [`Source::Tree`]
//! * `read_dir` + four probes per interface over `/sys/class/net`
//!   (wifi) — [`Source::Tree`], and the one that most needed it:
//!   `/sys/class/net/*/speed` dispatches the driver's `ethtool` op,
//!   which on some NICs blocks for hundreds of milliseconds and does so
//!   uninterruptibly. On a sampler thread that stretches one sampling
//!   interval. On the repaint thread it was a dropped frame at best and
//!   an evicted instrument at worst.
//! * `wpctl`, `nmcli` (sound, wifi) — [`Source::Command`], already
//!   off-thread via [`BackgroundCommand`], which this module keeps as
//!   that variant's backend rather than reimplementing.
//!
//! # Deliberately built-in only
//!
//! [`Source::Command`] is arbitrary-argv-by-declaration. The dock
//! executing an argv on a third party's behalf would blur exactly the
//! accountability line the out-of-process dockapp protocol is being
//! drawn to establish, so this registry is for built-in widgets and
//! stays that way; a dockapp runs its own process and does its own
//! sampling, which is the whole point of putting it in one.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wm_core::MouseButton;
use wm_theme_api::Point;

/// A sampling request. Widgets return these from
/// [`DockWidget::sources`](super::DockWidget::sources) at construction
/// and never see them again; the dock turns each one into a worker and
/// hands back the [`SourceId`] to read it by.
///
/// The interval is a request, not a guarantee. A sampler that takes
/// longer than its interval to complete one run simply runs less often
/// — the alternative (overlapping runs) would let a wedged `nmcli` or a
/// blocking `ethtool` op accumulate threads without bound.
pub enum Source {
    /// An external program, run to completion and its stdout kept.
    /// Backed by [`BackgroundCommand`].
    Command { program: &'static str, args: Vec<String>, interval: Duration },
    /// One file, read whole. `/proc` and `/sys` files are the intended
    /// use: small, synthesized on read, and — the part that matters —
    /// perfectly capable of blocking in the kernel.
    File { path: PathBuf, interval: Duration },
    /// `read_dir(root)`, then each name in `files` read inside every
    /// entry and each name in `dirs` tested for existence. Exists
    /// specifically so power and wifi stop walking sysfs on the repaint
    /// path; both need "one directory per device, a handful of tiny
    /// files each", which is neither one file nor a command.
    Tree { root: PathBuf, files: &'static [&'static str], dirs: &'static [&'static str], interval: Duration },
    /// The wall clock, truncated to `interval`. Not I/O — a clock read
    /// is a vDSO call, not a syscall — but it belongs here anyway so
    /// that a widget's whole input is one uniform thing it declares,
    /// and so the clock tile has no reason to keep any state of its own
    /// but the last value it drew.
    Clock { interval: Duration },
}

/// A widget's handle on one of its own [`Source`]s.
///
/// Opaque and dock-assigned: a widget receives its ids through
/// [`DockWidget::bind`](super::DockWidget::bind), in the same order it
/// listed the sources, and can do nothing with one but read it back out
/// of a [`Samples`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceId(u32);

impl SourceId {
    /// The id a widget field holds before `bind` has run — and, if a
    /// widget ever forgets to implement `bind`, forever.
    ///
    /// Every [`Samples`] accessor answers an unknown id with its empty
    /// value rather than panicking, so that mistake costs one instrument
    /// its readings and shows its dead face. A widget SDK whose
    /// misuse takes the whole shell down would be a worse bargain than
    /// the bug it was guarding against.
    pub const UNBOUND: SourceId = SourceId(u32::MAX);

    fn index(self) -> Option<usize> {
        (self != SourceId::UNBOUND).then_some(self.0 as usize)
    }
}

/// One directory inside a [`Source::Tree`] root — one power supply, one
/// network interface — with the requested files read and the requested
/// subdirectories tested.
///
/// `files` and `dirs` are positional against the `files`/`dirs` the
/// source declared, so a widget reads them by the index it wrote at
/// construction. Names rather than a map because both are three or four
/// entries long and fixed at compile time; a `HashMap` per device per
/// second would be pure ceremony.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeEntry {
    pub name: String,
    /// `None` where the file was absent or unreadable — sysfs serves
    /// EINVAL for plenty of files that exist, and a missing field must
    /// degrade only that field.
    pub files: Vec<Option<String>>,
    pub dirs: Vec<bool>,
}

impl TreeEntry {
    /// The `index`-th declared file's contents, if it read.
    pub fn file(&self, index: usize) -> Option<&str> {
        self.files.get(index).and_then(Option::as_deref)
    }

    /// Whether the `index`-th declared subdirectory exists.
    pub fn dir(&self, index: usize) -> bool {
        self.dirs.get(index).copied().unwrap_or(false)
    }
}

/// Everything a widget's sources have collected, as of this pass.
///
/// Borrowed from the registry's snapshot rather than owned, so a pass
/// over six widgets copies nothing: the registry clones a reading out
/// from under its sampler's mutex only on the pass where that sampler
/// actually produced a new one.
pub struct Samples<'a> {
    snapshot: &'a [Slot],
}

impl<'a> Samples<'a> {
    fn slot(&self, id: SourceId) -> Option<&'a Slot> {
        id.index().and_then(|index| self.snapshot.get(index))
    }

    /// The latest output of a [`Source::Command`] or [`Source::File`].
    /// `None` covers every way a reading can be absent — the command
    /// has not completed a run yet, it exited non-zero, the file did not
    /// read — because a widget's answer to all of them is the same: draw
    /// the dead face, do not invent a number.
    pub fn text(&self, id: SourceId) -> Option<&'a str> {
        match self.slot(id).map(|slot| &slot.reading) {
            Some(Reading::Text(text)) => Some(text.as_str()),
            _ => None,
        }
    }

    /// The latest walk of a [`Source::Tree`], sorted by entry name.
    /// Empty for an unreadable root, which is the same answer a genuinely
    /// empty root gives — neither tells a widget anything about the
    /// hardware it was looking for.
    pub fn tree(&self, id: SourceId) -> &'a [TreeEntry] {
        match self.slot(id).map(|slot| &slot.reading) {
            Some(Reading::Tree(entries)) => entries,
            _ => &[],
        }
    }

    /// The wall clock of a [`Source::Clock`], truncated to the interval
    /// it declared. Midnight for an unbound id, which is a wrong clock
    /// rather than a crashed shell — and visibly wrong, which is what a
    /// missing `bind` should look like.
    pub fn hms(&self, id: SourceId) -> (u32, u32, u32) {
        match self.slot(id).map(|slot| &slot.reading) {
            Some(&Reading::Clock(h, m, s)) => (h, m, s),
            _ => (0, 0, 0),
        }
    }

    /// The source is permanently unavailable and will not be retried —
    /// today's `BackgroundCommand::unusable`, which is set when the
    /// program could not be spawned at all. A missing binary is
    /// permanent within a session, so the worker stops and the widget
    /// can draw its "not available" face forever without paying a failed
    /// spawn per second.
    ///
    /// Only commands report it. A file or a directory that is missing
    /// now may exist after a hotplug (`/sys/class/net/wlan0` is the
    /// obvious case), so those sources keep looking and simply report
    /// nothing meanwhile.
    pub fn unusable(&self, id: SourceId) -> bool {
        self.slot(id).is_some_and(|slot| slot.unusable)
    }

    /// This source produced a new reading since the previous pass —
    /// today's `Sample::fresh`. Widgets fold on this rather than on a
    /// clock of their own: it is the difference between "the sampler
    /// completed a run" and "a sixtieth of a second went by", and only
    /// the first one is news.
    pub fn fresh(&self, id: SourceId) -> bool {
        self.slot(id).is_some_and(|slot| slot.fresh)
    }
}

/// Something a widget wants done that it is not allowed to do itself.
///
/// Returned from [`DockWidget::on_input`](super::DockWidget::on_input)
/// and executed by the dock. The point is symmetrical with [`Source`]:
/// a click that runs `wpctl set-volume` or `nmcli radio wifi off` is
/// every bit as capable of parking the repaint thread as a sample is,
/// so a widget declares the intent and the dock owns the thread.
pub enum Effect {
    /// This widget's pixels changed; redraw the dock.
    Repaint,
    /// Run a program off-thread, then — once it has exited — ask
    /// `then`'s sampler to sample immediately instead of waiting out
    /// its interval.
    ///
    /// `then` is what makes a control tile feel connected without ever
    /// trusting the command's exit status: the authority on what a
    /// click did is the next sample, and this just asks for that sample
    /// as soon as the command lands rather than up to an interval later.
    Run { program: &'static str, args: Vec<String>, then: Option<SourceId> },
    /// Ask a sampler to sample now. For the case with no command in
    /// front of it.
    Resample(SourceId),
}

/// A pointer event, in the coordinates of the tile it landed on.
///
/// Press/release rather than a single "click", and scroll and
/// enter/leave alongside them, because this enum is the shape the
/// out-of-process dockapp protocol needs to carry and it is cheap to
/// settle now, with six implementors in one crate, rather than after a
/// protocol version has shipped with the narrower one baked in.
///
/// Not everything here is emitted yet, and not every button reaches a
/// widget.
///
/// The dock delivers `Press` and `Release` for [`MouseButton::Left`]
/// only. Middle is the drag-to-reorder gesture and right is reserved
/// for a per-tile menu; both are the dock's, and a widget that had
/// already been given one could not have it taken back — so a widget
/// may assume `button` is `Left` today, and must not assume it will
/// stay the only one.
///
/// `Scroll` needs a `Backend::take_shell_scroll` in both backends (the
/// X11 side reads buttons 4/5, the Wayland side an axis event) and
/// `Enter`/`Leave` need the dock to track which slot the pointer is
/// over across motion. Both are the dockapp phase's work, and both are
/// additions to the dock, not to this enum — which is exactly why the
/// enum carries them now.
pub enum DockInput {
    Press { local: Point, button: MouseButton },
    Release { local: Point, button: MouseButton },
    Scroll { local: Point, delta: i32 },
    Enter,
    Leave,
}

impl DockInput {
    /// Where in the dock's column this landed, for the variants that
    /// have a position at all. `Enter`/`Leave` are about the tile as a
    /// whole, which is why they carry none.
    pub fn local(&self) -> Option<Point> {
        match *self {
            DockInput::Press { local, .. } | DockInput::Release { local, .. } | DockInput::Scroll { local, .. } => Some(local),
            DockInput::Enter | DockInput::Leave => None,
        }
    }

    /// Re-anchors this input from dock-local to tile-local coordinates,
    /// so a widget can carve its face into control zones without
    /// knowing where in the column the dock stacked it.
    pub(crate) fn translated(self, origin: Point) -> DockInput {
        let shift = |local: Point| Point::new(local.x - origin.x, local.y - origin.y);
        match self {
            DockInput::Press { local, button } => DockInput::Press { local: shift(local), button },
            DockInput::Release { local, button } => DockInput::Release { local: shift(local), button },
            DockInput::Scroll { local, delta } => DockInput::Scroll { local: shift(local), delta },
            other => other,
        }
    }
}

/// One sampler's contribution to the current pass.
#[derive(Debug, Default)]
struct Slot {
    reading: Reading,
    unusable: bool,
    fresh: bool,
}

/// The last thing a sampler produced. Retained across passes: a widget
/// that folds only on `fresh` still wants the value there on the passes
/// in between (`power` compares against it, `net` renders from it), and
/// keeping it is what lets the registry clone out of a sampler's mutex
/// only on the pass its generation actually moved.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Reading {
    #[default]
    Missing,
    Text(String),
    Tree(Vec<TreeEntry>),
    Clock(u32, u32, u32),
}

/// Every source every widget declared, one worker each, plus the
/// snapshot [`Samples`] borrows from.
///
/// Built-in only, on purpose — see the module docs.
pub(crate) struct SamplerRegistry {
    samplers: Vec<Sampler>,
    /// Per source, the sampler generation already folded into
    /// `snapshot`. Parallel to `samplers`, as is `snapshot`; three
    /// vectors indexed by [`SourceId`] rather than one vector of
    /// structs, because `snapshot` is what `Samples` borrows and it must
    /// not drag a worker handle into that borrow.
    seen: Vec<u64>,
    snapshot: Vec<Slot>,
}

enum Sampler {
    /// [`Source::Command`] and [`Source::File`] both. They differ only
    /// in how the worker produces its string; from the registry's side,
    /// and from a widget's, they are the same thing.
    Text(Worker<String>),
    Tree(Worker<Vec<TreeEntry>>),
    /// No worker: reading the wall clock is a vDSO call costing tens of
    /// nanoseconds, so a thread and a mutex to carry it across would be
    /// more machinery than the thing being carried. `granularity` is
    /// the declared interval in whole seconds, and truncating to it is
    /// what makes the interval mean something: a one-second clock ticks
    /// on the second, and a sixty-second one goes `fresh` once a minute.
    Clock { granularity: u64 },
}

impl SamplerRegistry {
    pub(crate) fn new() -> Self {
        Self { samplers: Vec::new(), seen: Vec::new(), snapshot: Vec::new() }
    }

    /// Starts a worker per source and hands back the ids to read them
    /// by, positionally matching `sources`. Called once per widget, at
    /// the one place widgets enter the dock.
    pub(crate) fn register(&mut self, sources: Vec<Source>) -> Vec<SourceId> {
        sources
            .into_iter()
            .map(|source| {
                let sampler = match source {
                    Source::Command { program, args, interval } => Sampler::Text(BackgroundCommand::spawn(program, args, interval).worker()),
                    Source::File { path, interval } => Sampler::Text(spawn_file_worker(path, interval)),
                    Source::Tree { root, files, dirs, interval } => Sampler::Tree(spawn_tree_worker(root, files, dirs, interval)),
                    // `max(1)` rather than an error: a widget asking for
                    // sub-second granularity from a tile that draws a
                    // second hand is asking for something the face
                    // cannot show, and rounding it up is a better answer
                    // than a modulo by zero.
                    Source::Clock { interval } => Sampler::Clock { granularity: interval.as_secs().max(1) },
                };
                self.samplers.push(sampler);
                self.seen.push(0);
                self.snapshot.push(Slot::default());
                SourceId((self.samplers.len() - 1) as u32)
            })
            .collect()
    }

    /// Pulls whatever the workers have finished into the snapshot and
    /// recomputes `fresh`. Exactly once per widget pass, so `fresh`
    /// means "new since the last `update`" for every widget alike —
    /// which is only unambiguous because sources are never shared
    /// between widgets.
    ///
    /// Never blocks on a sampler: the only lock taken is that sampler's
    /// own mutex, which its worker holds solely to swap in a finished
    /// result.
    pub(crate) fn refresh(&mut self) {
        for (index, sampler) in self.samplers.iter().enumerate() {
            let seen = &mut self.seen[index];
            let slot = &mut self.snapshot[index];
            match sampler {
                Sampler::Text(worker) => match worker.take_if_new(seen) {
                    Some(fresh) => {
                        slot.reading = fresh.reading.map_or(Reading::Missing, Reading::Text);
                        slot.unusable = fresh.unusable;
                        slot.fresh = true;
                    }
                    None => slot.fresh = false,
                },
                Sampler::Tree(worker) => match worker.take_if_new(seen) {
                    Some(fresh) => {
                        slot.reading = fresh.reading.map_or(Reading::Missing, Reading::Tree);
                        slot.unusable = fresh.unusable;
                        slot.fresh = true;
                    }
                    None => slot.fresh = false,
                },
                Sampler::Clock { granularity } => {
                    let (h, m, s) = wall_clock(*granularity);
                    let reading = Reading::Clock(h, m, s);
                    slot.fresh = slot.reading != reading;
                    slot.reading = reading;
                }
            }
        }
    }

    /// The current pass's readings. Borrows the snapshot, not the
    /// samplers, so the widget loop can hold this while mutating the
    /// widget list beside it.
    pub(crate) fn samples(&self) -> Samples<'_> {
        Samples { snapshot: &self.snapshot }
    }

    /// A thread-safe nudge for one source, or `None` for a clock (which
    /// has no worker to wake and is never behind).
    pub(crate) fn resampler(&self, id: SourceId) -> Option<Resampler> {
        match id.index().and_then(|index| self.samplers.get(index))? {
            Sampler::Text(worker) => Some(worker.resampler()),
            Sampler::Tree(worker) => Some(worker.resampler()),
            Sampler::Clock { .. } => None,
        }
    }
}

/// The wall clock as `(h, m, s)`, truncated to `granularity` seconds.
fn wall_clock(granularity: u64) -> (u32, u32, u32) {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let today = secs % 86_400;
    let today = today - today % granularity;
    ((today / 3600) as u32, ((today % 3600) / 60) as u32, (today % 60) as u32)
}

/// One external command, run on a thread of its own and never on the
/// caller's.
///
/// # Why this exists
///
/// A widget that samples the system by shelling out used to do it
/// inline in `tick()`, and `tick()` is called from the compositor's
/// repaint path. That makes the slowest external command on the system
/// a hard bound on the desktop's frame rate. It is not a theoretical
/// bound: `nmcli dev wifi` defaults to `--rescan auto`, which blocks for
/// a full hardware scan whenever NetworkManager's cache is older than
/// thirty seconds. On the machine this was diagnosed on that was ~3.6
/// seconds, once every ~34 seconds, and for the whole of it the
/// compositor was parked in `waitpid` — not drawing, not reading input,
/// and not collecting the page-flip completion already sitting in the
/// DRM fd. The compositor's own stall watchdog then blamed the display
/// driver, which was innocent and idle.
///
/// A slow child process should cost a stale tile, never a frozen
/// screen. So the command runs on a worker thread and the widget reads
/// whatever the last completed run produced.
///
/// # Contract
///
/// Sampling is *pure output collection*: the worker gets an argv and
/// hands back stdout. Parsing, and every decision that depends on
/// widget state, stays on the widget thread where that state lives.
/// This deliberately keeps the shared surface to one `String`.
///
/// # Its place now
///
/// This type is [`Source::Command`]'s backend and no longer something a
/// widget constructs. That is the generalization the incident argued
/// for: it was already true that a command must not run on the repaint
/// thread, and the only thing left to fix was that a widget still had
/// to *remember* it. Now it declares a `Source` and one of these
/// appears behind it — along with a [`Source::File`] and a
/// [`Source::Tree`] built on the same worker, because a `read` and a
/// `read_dir` can block that thread exactly as well as a `waitpid` can.
pub(crate) struct BackgroundCommand(Worker<String>);

impl BackgroundCommand {
    /// Starts the worker. The thread is detached and runs for the life
    /// of the process: widgets are created once at startup and live in
    /// the dock until the session ends, so there is no teardown path to
    /// serve and a join handle would only be something to drop.
    pub(crate) fn spawn(program: &'static str, args: Vec<String>, interval: Duration) -> Self {
        Self(Worker::spawn(format!("chonkstep-sample-{program}"), interval, move || {
            // The one place in this crate where blocking on a child
            // process is the *point*: this closure is the body of the
            // sampler thread `Worker::spawn` started, so the only thing
            // `output()` can park here is this worker. That is exactly
            // the property `clippy.toml`'s ban on `Command::output`
            // exists to force someone to state out loud — see
            // `super::SupervisedWidget` for what happens to a widget
            // that gets it wrong and blocks the repaint thread instead.
            #[allow(clippy::disallowed_methods)]
            let result = Command::new(program).args(&args).output();
            match result {
                // A failed run clears the reading rather than leaving
                // the last good one on screen: a tile showing a network
                // that went away is worse than a tile admitting it does
                // not know.
                Ok(output) => Outcome::Sampled(output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())),
                Err(error) => {
                    tracing::debug!(?error, program, "sampler command could not be spawned; giving up on it");
                    Outcome::Unusable
                }
            }
        }))
    }

    /// Unwraps to the worker the registry stores. `Source::Command` is
    /// the only thing that constructs one of these and the registry the
    /// only thing that holds one, so the type exists for its name and
    /// the argument in its doc comment rather than for an API.
    fn worker(self) -> Worker<String> {
        self.0
    }
}

/// [`Source::File`]'s backend: one `read_to_string` per interval, on a
/// worker thread.
///
/// Never `Unusable`. A command's binary either exists or does not, but
/// a file's absence is routinely temporary — `/sys/class/net/wlan0`
/// appears when a USB dongle is plugged in — so this keeps looking and
/// reports `Missing` in between. The cost of being wrong in this
/// direction is one `openat` per second that returns ENOENT.
fn spawn_file_worker(path: PathBuf, interval: Duration) -> Worker<String> {
    Worker::spawn("chonkstep-sample-file".to_string(), interval, move || {
        // Blocking `read` on a procfs or sysfs file is the point here,
        // for the same reason `Command::output` is above: this closure
        // *is* the worker thread. `/proc` files are synthesized by the
        // kernel on read and can take a seqlock or a subsystem lock on
        // the way; the repaint thread is the one place that must never
        // wait on one.
        Outcome::Sampled(std::fs::read_to_string(&path).ok())
    })
}

/// [`Source::Tree`]'s backend.
///
/// The interesting one for latency. `/sys/class/net/*/speed` dispatches
/// into the driver's `ethtool` `get_link_ksettings` op, which on some
/// NICs blocks for hundreds of milliseconds and is not interruptible;
/// `/sys/class/power_supply/*/capacity` can go out to an embedded
/// controller over I2C. Both are fine here — a slow run stretches this
/// worker's interval and nothing else — and both were, until this
/// landed, executed once a second on the thread that draws the screen.
fn spawn_tree_worker(root: PathBuf, files: &'static [&'static str], dirs: &'static [&'static str], interval: Duration) -> Worker<Vec<TreeEntry>> {
    Worker::spawn("chonkstep-sample-tree".to_string(), interval, move || Outcome::Sampled(Some(read_tree(&root, files, dirs))))
}

/// The whole of a [`Source::Tree`] walk, split out from its worker so
/// it can be tested against a fixture directory.
///
/// Sorted by name because `read_dir` promises no ordering, and a widget
/// that lets a click cycle through the entries (wifi does) needs the
/// order to be the same on the next sample as it was on this one.
fn read_tree(root: &Path, files: &[&str], dirs: &[&str]) -> Vec<TreeEntry> {
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    let mut out: Vec<TreeEntry> = entries
        .flatten()
        .map(|entry| {
            let dir = entry.path();
            TreeEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                // Per-field degradation, not per-entry: a supply
                // directory with an unreadable `capacity` still
                // contributes its `status`, and an interface whose
                // driver refuses `speed` while the link is down still
                // contributes its `operstate`.
                files: files.iter().map(|file| std::fs::read_to_string(dir.join(file)).ok()).collect(),
                dirs: dirs.iter().map(|sub| dir.join(sub).exists()).collect(),
            }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Runs one program on a thread of its own and optionally nudges a
/// sampler when it exits — the executor half of [`Effect::Run`].
///
/// This is the click path's version of the whole argument: `wpctl
/// set-volume` and `nmcli radio wifi off` arrive on the same repaint
/// thread a sample would have, and are just as able to park it.
pub(crate) fn run_detached(program: &'static str, args: Vec<String>, then: Option<Resampler>) {
    std::thread::Builder::new()
        .name(format!("chonkstep-run-{program}"))
        .spawn(move || {
            // Audited exception to `clippy.toml`'s ban, and the one
            // worth reading twice: `nmcli` reaches this line, and
            // `nmcli` is the exact binary whose blocking call froze the
            // desktop on 2026-08-29. It is safe here for one reason
            // only — this closure is the body of this effect's own
            // worker thread, never the compositor's repaint loop. The
            // widget that asked for it returned an `Effect` and cannot
            // have run anything itself.
            #[allow(clippy::disallowed_methods)]
            let _ = Command::new(program).args(&args).output();
            if let Some(resampler) = then {
                resampler.resample_soon();
            }
        })
        .map_err(|error| tracing::warn!(?error, program, "could not start the effect thread; the command will not run"))
        .ok();
}

/// The sampler thread and the mailbox it drops results into, shared by
/// every [`Source`] variant that needs one.
///
/// Generic over the reading rather than duplicated per source kind: the
/// interval loop, the condvar wake, the poisoned-lock handling and the
/// generation counter are the parts that are easy to get subtly wrong,
/// and there is exactly one copy of them.
struct Worker<T> {
    shared: Arc<Shared<T>>,
}

struct Shared<T> {
    state: Mutex<SampleState<T>>,
    /// Signals the worker to sample immediately instead of sleeping out
    /// the rest of its interval — see [`Resampler`].
    wake: Condvar,
}

struct SampleState<T> {
    /// The most recent successful run's reading. `None` before the
    /// first one completes, and again after a run that failed.
    reading: Option<T>,
    /// Set once the source could not be reached at all and will not be
    /// retried. Only commands ever set it; see [`spawn_file_worker`].
    unusable: bool,
    /// Bumped on every completed run so a reader can distinguish a
    /// fresh reading from the one it already consumed.
    generation: u64,
    /// Set by [`Resampler::resample_soon`], cleared by the worker when
    /// it acts on it.
    resample_now: bool,
}

/// What one run of a sampler produced.
enum Outcome<T> {
    /// A run completed. `None` means it completed without a usable
    /// reading (non-zero exit, unreadable file) — which clears the tile
    /// rather than leaving a stale number on it.
    Sampled(Option<T>),
    /// The source cannot be reached at all; stop the worker. Permanent
    /// within a session by construction, since the only thing that
    /// produces it is a spawn failure.
    Unusable,
}

/// A completed run, cloned out from under the worker's mutex.
struct FreshReading<T> {
    reading: Option<T>,
    unusable: bool,
}

impl<T: Clone + Send + 'static> Worker<T> {
    /// Starts the worker. The thread is detached and runs for the life
    /// of the process: widgets are created once at startup and live in
    /// the dock until the session ends, so there is no teardown path to
    /// serve and a join handle would only be something to drop.
    fn spawn(thread_name: String, interval: Duration, sample: impl FnMut() -> Outcome<T> + Send + 'static) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(SampleState { reading: None, unusable: false, generation: 0, resample_now: false }),
            wake: Condvar::new(),
        });
        let worker = Arc::clone(&shared);
        std::thread::Builder::new()
            .name(thread_name.clone())
            .spawn(move || sample_loop(worker, sample, interval))
            // A system that cannot start a thread is not one this
            // widget can improve on by panicking the compositor. The
            // widget simply never sees a sample and shows its dead face.
            .map_err(|error| tracing::warn!(?error, thread = %thread_name, "could not start the sampler thread"))
            .ok();
        Self { shared }
    }

    /// The latest completed run, or `None` if none has completed since
    /// `seen`. Never blocks on the source; the only lock held is the
    /// sampler's own mutex, and the worker holds it solely to swap in a
    /// finished result.
    fn take_if_new(&self, seen: &mut u64) -> Option<FreshReading<T>> {
        let state = match self.shared.state.lock() {
            Ok(state) => state,
            // A panicking sampler thread must not take the desktop with
            // it: treat a poisoned lock as "no data", same as a source
            // that has not produced anything yet.
            Err(poisoned) => poisoned.into_inner(),
        };
        if state.generation == *seen {
            return None;
        }
        *seen = state.generation;
        Some(FreshReading { reading: state.reading.clone(), unusable: state.unusable })
    }

    fn resampler(&self) -> Resampler {
        Resampler { shared: Arc::clone(&self.shared) as Arc<dyn Wake> }
    }
}

/// The one thing a [`Resampler`] can do, as a trait so one handle type
/// serves every reading type.
trait Wake: Send + Sync {
    fn resample_soon(&self);
}

impl<T: Send> Wake for Shared<T> {
    fn resample_soon(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.resample_now = true;
            self.wake.notify_all();
        }
    }
}

/// A thread-safe handle that can only ask for a resample.
///
/// The click paths run their `set` command off-thread — a volume change
/// or a radio toggle is just as capable of blocking as a sample is —
/// and want the tile to catch up when that command lands rather than up
/// to an interval later. The authority on what a click actually did is
/// the next sample, never the command's exit status.
#[derive(Clone)]
pub(crate) struct Resampler {
    shared: Arc<dyn Wake>,
}

impl Resampler {
    pub(crate) fn resample_soon(&self) {
        self.shared.resample_soon();
    }
}

fn sample_loop<T>(shared: Arc<Shared<T>>, mut sample: impl FnMut() -> Outcome<T>, interval: Duration) {
    loop {
        match sample() {
            Outcome::Sampled(reading) => {
                if let Ok(mut state) = shared.state.lock() {
                    state.reading = reading;
                    state.generation = state.generation.wrapping_add(1);
                }
            }
            Outcome::Unusable => {
                if let Ok(mut state) = shared.state.lock() {
                    state.reading = None;
                    state.unusable = true;
                    state.generation = state.generation.wrapping_add(1);
                }
                return;
            }
        }

        let Ok(mut state) = shared.state.lock() else { return };
        state.resample_now = false;
        // Condvar rather than a plain sleep so a click can shorten the
        // wait. The timeout is the normal path; the notify is the
        // exception.
        while !state.resample_now {
            let (next, timeout) = match shared.wake.wait_timeout(state, interval) {
                Ok(pair) => pair,
                Err(_) => return,
            };
            state = next;
            if timeout.timed_out() {
                break;
            }
        }
    }
}

/// Hand-built [`Samples`] for widget tests.
///
/// The entire value of Layer 3 to a test is that a widget's input is
/// now data: `update` over a fixed `Samples` is a pure function, so
/// every widget's fold can be exercised against synthetic `/proc`
/// contents and synthetic sysfs trees with no kernel, no `nmcli`, and
/// no clock. This is the thing that makes that convenient.
#[cfg(test)]
pub(crate) struct SampleBench {
    snapshot: Vec<Slot>,
}

#[cfg(test)]
impl SampleBench {
    pub(crate) fn new() -> Self {
        Self { snapshot: Vec::new() }
    }

    fn push(&mut self, reading: Reading) -> SourceId {
        self.snapshot.push(Slot { reading, unusable: false, fresh: true });
        SourceId((self.snapshot.len() - 1) as u32)
    }

    /// A command/file source holding `contents`, marked fresh.
    pub(crate) fn text(&mut self, contents: &str) -> SourceId {
        self.push(Reading::Text(contents.to_string()))
    }

    /// A command/file source that has produced nothing.
    pub(crate) fn missing(&mut self) -> SourceId {
        self.push(Reading::Missing)
    }

    /// A command source whose program could not be spawned.
    pub(crate) fn unusable(&mut self) -> SourceId {
        let id = self.push(Reading::Missing);
        self.snapshot[id.0 as usize].unusable = true;
        id
    }

    pub(crate) fn tree(&mut self, entries: Vec<TreeEntry>) -> SourceId {
        self.push(Reading::Tree(entries))
    }

    pub(crate) fn clock(&mut self, hms: (u32, u32, u32)) -> SourceId {
        self.push(Reading::Clock(hms.0, hms.1, hms.2))
    }

    /// Replaces a source's reading and marks it fresh — one more
    /// sampler run, which is what a widget folds on.
    pub(crate) fn set_text(&mut self, id: SourceId, contents: &str) {
        self.snapshot[id.0 as usize] = Slot { reading: Reading::Text(contents.to_string()), unusable: false, fresh: true };
    }

    pub(crate) fn set_tree(&mut self, id: SourceId, entries: Vec<TreeEntry>) {
        self.snapshot[id.0 as usize] = Slot { reading: Reading::Tree(entries), unusable: false, fresh: true };
    }

    pub(crate) fn set_clock(&mut self, id: SourceId, hms: (u32, u32, u32)) {
        self.snapshot[id.0 as usize] = Slot { reading: Reading::Clock(hms.0, hms.1, hms.2), unusable: false, fresh: true };
    }

    /// Marks every source stale: the pass where no sampler completed a
    /// run, which is the overwhelmingly common one at 60Hz against a
    /// 1Hz source and the one a widget must fold to "nothing changed".
    pub(crate) fn all_stale(&mut self) {
        for slot in &mut self.snapshot {
            slot.fresh = false;
        }
    }

    pub(crate) fn samples(&self) -> Samples<'_> {
        Samples { snapshot: &self.snapshot }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every accessor answers an id it has never heard of with its
    /// empty value. A widget that forgets `bind` must cost one dead
    /// instrument, not a panicking shell — see [`SourceId::UNBOUND`].
    #[test]
    fn an_unbound_source_reads_as_empty_rather_than_panicking() {
        let bench = SampleBench::new();
        let samples = bench.samples();
        assert_eq!(samples.text(SourceId::UNBOUND), None);
        assert!(samples.tree(SourceId::UNBOUND).is_empty());
        assert_eq!(samples.hms(SourceId::UNBOUND), (0, 0, 0));
        assert!(!samples.unusable(SourceId::UNBOUND));
        assert!(!samples.fresh(SourceId::UNBOUND));
        // And an id from a *different* registry, which is the same
        // mistake with a plausible-looking number in it.
        assert_eq!(samples.text(SourceId(7)), None);
    }

    #[test]
    fn accessors_only_answer_for_their_own_source_kind() {
        let mut bench = SampleBench::new();
        let text = bench.text("hello");
        let tree = bench.tree(vec![TreeEntry { name: "BAT0".into(), files: vec![Some("Battery".into())], dirs: vec![true] }]);
        let clock = bench.clock((13, 30, 5));
        let samples = bench.samples();

        assert_eq!(samples.text(text), Some("hello"));
        assert_eq!(samples.text(tree), None, "a tree is not text");
        assert_eq!(samples.hms(text), (0, 0, 0), "text is not a clock");
        assert_eq!(samples.tree(tree).len(), 1);
        assert_eq!(samples.tree(tree)[0].file(0), Some("Battery"));
        assert_eq!(samples.tree(tree)[0].file(9), None, "an out-of-range field is absent, not a panic");
        assert!(samples.tree(tree)[0].dir(0));
        assert!(!samples.tree(tree)[0].dir(9));
        assert_eq!(samples.hms(clock), (13, 30, 5));
    }

    #[test]
    fn unusable_is_reported_and_carries_no_reading() {
        let mut bench = SampleBench::new();
        let id = bench.unusable();
        let samples = bench.samples();
        assert!(samples.unusable(id));
        assert_eq!(samples.text(id), None);
    }

    /// The sysfs walk with holes in it, at its new home. This used to
    /// be `power.rs`'s `read_supplies_from` test; the walk moved onto a
    /// sampler thread, and the coverage moved with it.
    #[test]
    fn read_tree_walks_a_fixture_directory_and_degrades_per_field() {
        let root = std::env::temp_dir().join(format!("chonkstep-tree-fixture-{}", std::process::id()));
        let bat = root.join("BAT0");
        let ac = root.join("AC");
        std::fs::create_dir_all(&bat).unwrap();
        std::fs::create_dir_all(&ac).unwrap();
        std::fs::write(bat.join("type"), "Battery\n").unwrap();
        std::fs::write(bat.join("capacity"), "73\n").unwrap();
        // No status file at all: the field must degrade, not the entry.
        std::fs::write(ac.join("type"), "Mains\n").unwrap();
        std::fs::write(ac.join("online"), "1\n").unwrap();
        std::fs::create_dir_all(ac.join("device")).unwrap();

        let entries = read_tree(&root, &["type", "capacity", "status", "online"], &["device"]);
        // Sorted by name: "AC" before "BAT0", whatever order the
        // filesystem handed them back in.
        assert_eq!(entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), vec!["AC", "BAT0"]);
        assert_eq!(entries[0].file(0), Some("Mains\n"));
        assert_eq!(entries[0].file(3), Some("1\n"));
        assert_eq!(entries[0].file(1), None, "the AC supply has no capacity file");
        assert!(entries[0].dir(0));
        assert_eq!(entries[1].file(0), Some("Battery\n"));
        assert_eq!(entries[1].file(1), Some("73\n"));
        assert_eq!(entries[1].file(2), None, "the missing status file degrades to None");
        assert!(!entries[1].dir(0), "BAT0 has no device subdirectory");

        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(read_tree(&root, &["type"], &[]), Vec::new(), "a missing root reads as no entries");
    }

    /// The registry's clock has no worker and no interval timer: it is
    /// evaluated every pass and reports `fresh` when the truncated value
    /// actually moved. That is both cheaper than a thread and more
    /// accurate than a throttle would be — a one-second throttle started
    /// at an arbitrary moment ticks the second hand up to a second late.
    #[test]
    fn a_clock_source_goes_fresh_exactly_when_its_truncated_value_moves() {
        let mut registry = SamplerRegistry::new();
        let ids = registry.register(vec![Source::Clock { interval: Duration::from_secs(1) }]);
        let id = ids[0];

        registry.refresh();
        assert!(registry.samples().fresh(id), "the first pass is always news");
        let first = registry.samples().hms(id);
        registry.refresh();
        assert!(!registry.samples().fresh(id), "a second pass in the same second is not");
        assert_eq!(registry.samples().hms(id), first, "and the value is retained across it");
    }

    #[test]
    fn a_clock_sources_interval_truncates_the_reading() {
        let minute = wall_clock(60);
        assert_eq!(minute.2, 0, "a minute-granularity clock never reports seconds");
        let hour = wall_clock(3600);
        assert_eq!((hour.1, hour.2), (0, 0));
        // Truncation must not round a second past its own minute.
        assert!(wall_clock(1).2 < 60);
    }

    /// Registration is positional and ids are stable across widgets:
    /// the second widget's first source must not collide with the
    /// first widget's.
    #[test]
    fn ids_are_assigned_in_order_and_never_reused_across_widgets() {
        let mut registry = SamplerRegistry::new();
        let first = registry.register(vec![Source::Clock { interval: Duration::from_secs(1) }, Source::Clock { interval: Duration::from_secs(60) }]);
        let second = registry.register(vec![Source::Clock { interval: Duration::from_secs(1) }]);
        assert_eq!(first, vec![SourceId(0), SourceId(1)]);
        assert_eq!(second, vec![SourceId(2)]);
        assert!(registry.resampler(first[0]).is_none(), "a clock has no worker to nudge");
    }

    /// A file source that cannot read reports nothing and stays alive.
    /// The distinction from `unusable` is the point: a missing binary
    /// is permanent, a missing sysfs path is a dongle that has not been
    /// plugged in yet.
    #[test]
    fn a_missing_file_source_is_absent_but_never_unusable() {
        let mut registry = SamplerRegistry::new();
        let ids = registry.register(vec![Source::File {
            path: PathBuf::from("/nonexistent/chonkstep/definitely-not-here"),
            interval: Duration::from_millis(5),
        }]);
        let id = ids[0];
        // Spin until the worker's first run has landed rather than
        // sleeping a fixed time: this test must not be a race on a
        // loaded runner, and the worker's first read happens
        // immediately (the interval is the gap *between* runs).
        for _ in 0..2_000 {
            registry.refresh();
            if registry.samples().fresh(id) {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(registry.samples().text(id), None);
        assert!(!registry.samples().unusable(id), "a file that is not there yet may be there later");
    }

    /// The happy path of a file source, end to end through a real
    /// worker thread: a file on disk becomes a `Samples::text`.
    #[test]
    fn a_file_source_delivers_its_contents_through_a_worker_thread() {
        let path = std::env::temp_dir().join(format!("chonkstep-file-fixture-{}", std::process::id()));
        std::fs::write(&path, "MemTotal: 1 kB\n").unwrap();

        let mut registry = SamplerRegistry::new();
        let id = registry.register(vec![Source::File { path: path.clone(), interval: Duration::from_millis(5) }])[0];
        let mut text = None;
        for _ in 0..2_000 {
            registry.refresh();
            if let Some(contents) = registry.samples().text(id) {
                text = Some(contents.to_string());
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        std::fs::remove_file(&path).ok();
        assert_eq!(text.as_deref(), Some("MemTotal: 1 kB\n"));
    }

    /// A program that is not on the system is `unusable`, once, and
    /// then its worker stops — a missing binary is permanent within a
    /// session, and retrying it once a second forever would be a failed
    /// spawn per second for nothing.
    #[test]
    fn a_command_that_cannot_spawn_is_unusable_and_stops_trying() {
        let mut registry = SamplerRegistry::new();
        let id = registry.register(vec![Source::Command {
            program: "chonkstep-no-such-program-exists",
            args: Vec::new(),
            interval: Duration::from_millis(5),
        }])[0];
        for _ in 0..2_000 {
            registry.refresh();
            if registry.samples().unusable(id) {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(registry.samples().unusable(id));
        assert_eq!(registry.samples().text(id), None);

        // The worker returned, so nothing bumps the generation again:
        // the flag persists without a single further spawn attempt.
        registry.refresh();
        assert!(!registry.samples().fresh(id), "a stopped worker produces no more readings");
        assert!(registry.samples().unusable(id), "and the verdict it left behind stands");
    }

    /// The regression guard for the incident itself, stated as the
    /// property rather than the symptom.
    ///
    /// `refresh` is what runs on the compositor's repaint thread. A
    /// sampler parked in a child process that will not return for two
    /// seconds must not cost that thread anything at all — which is the
    /// exact opposite of what `nmcli dev wifi` did from `tick()` on
    /// 2026-08-29, when a ~3.6s scan became a ~3.6s freeze.
    ///
    /// The bound is one whole frame (16ms, `HOUSEKEEPING_INTERVAL`) for
    /// a *thousand* refreshes taken while the child is parked — chosen
    /// loose enough that a debug build on a loaded runner cannot fail
    /// it by accident, and still tighter than the failure it guards
    /// against by more than two orders of magnitude.
    #[test]
    fn a_sampler_blocked_in_a_child_process_costs_the_caller_nothing() {
        let mut registry = SamplerRegistry::new();
        registry.register(vec![Source::Command {
            program: "sleep",
            args: vec!["2".to_string()],
            interval: Duration::from_millis(1),
        }]);
        // Let the worker actually reach the child before measuring, so
        // this times refreshes taken *while* it is blocked.
        std::thread::sleep(Duration::from_millis(50));

        let start = std::time::Instant::now();
        for _ in 0..1_000 {
            registry.refresh();
            let _ = registry.samples();
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(16),
            "1000 refreshes took {elapsed:?} while a sampler was parked in a 2s child; the whole point of \
             this module is that the repaint thread never waits for one"
        );
    }

    /// The same claim for the click path: `Effect::Run` hands the
    /// command to a thread and returns, so a `wpctl` or `nmcli` that
    /// hangs costs a tile that does not catch up rather than a desktop
    /// that stops drawing.
    #[test]
    fn running_an_effect_returns_before_the_command_does() {
        let start = std::time::Instant::now();
        run_detached("sleep", vec!["2".to_string()], None);
        assert!(start.elapsed() < Duration::from_millis(50), "run_detached must not wait on the child");
    }
}
