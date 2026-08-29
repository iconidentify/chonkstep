//! The dock's side of the widget SDK: supervision, and the re-exports
//! that let the rest of the shell name a widget without reaching past
//! it.
//!
//! The SDK itself is two crates, and the split is load-bearing:
//!
//! * `chonk-dock-widget` is the vocabulary — the [`DockWidget`] trait,
//!   [`Source`], [`Samples`], [`Effect`], [`DockInput`]. Nothing in it
//!   can perform I/O.
//! * `chonk-instruments` is the six built-in tiles, written against
//!   that vocabulary and against `wm-theme`, and *nothing else*. Its
//!   own `clippy.toml` makes `std::fs::File`, `std::process::Command`,
//!   `std::fs::{read, read_to_string, read_dir}`, `std::thread::spawn`
//!   and `TcpStream::connect` build errors inside it.
//!
//! What is left in this module is what belongs to the dock rather than
//! to a widget: [`sampling`], the runtime that turns declared `Source`s
//! into `Samples` on threads of its own, and [`SupervisedWidget`], the
//! timing guard the dock wraps every item in.
//!
//! Read the two together and the layering is the whole story. Sampling
//! is a structure in which a widget cannot freeze the compositor —
//! there is no entry point from which it could — and the extraction
//! makes writing one anyway a compile error. Supervision is the
//! backstop for everything that structure does not cover: an
//! accidentally quadratic render, a pathological text-shaping path, an
//! out-of-process tile whose code is not in this repository at all.
//! Keep both; do not confuse them.

use std::time::{Duration, Instant};

use wm_theme::Theme;
use wm_theme_api::DecorationBuffer;

pub mod sampling;

pub use chonk_dock_widget::{DockInput, DockWidget, Effect, Samples, Source, SourceId, TreeEntry, SAMPLE_INTERVAL};
pub use chonk_instruments::{ClockWidget, NetTrafficWidget, PowerWidget, SoundWidget, SysLoadWidget, WifiWidget};

pub(crate) use sampling::{run_detached, SamplerRegistry};

/// The reserved namespace for a built-in tile's id.
///
/// Dock order is persisted as one id per line (see
/// [`crate::desktop::dock_order`]), and that file names both kinds of
/// tile. A remote tile's id is the `id` field of the `.dockapp` file
/// that declared it, which is attacker-chosen in the weak sense that
/// anything on disk is; prefixing the built-ins keeps a `.dockapp`
/// claiming `id = "clock"` from displacing the analog clock, and keeps
/// the two namespaces separable by eye when a user edits the file by
/// hand — which is a supported thing to do, exactly as it is for the
/// launcher's pin file beside it.
pub(crate) const BUILTIN_PREFIX: &str = "builtin:";

/// One thing stacked in the dock's column.
///
/// # Why this exists at all
///
/// The dock is about to hold two very different kinds of tile: the six
/// instruments that ship with the compositor, and out-of-process
/// dockapps that push pixels down a socket
/// (`chonk-dock-proto`). Almost nothing in the dock cares which is
/// which. Layout walks a column of heights; `redraw_dock` blits a
/// column of buffers; a middle-drag swaps two positions; a click
/// resolves to a slot and hands it a tile-local event. Every one of
/// those is the same code for both.
///
/// So the split is an enum behind the *existing* trait rather than a
/// second trait, a generic parameter, or a parallel `Vec`. This type
/// implements [`DockWidget`], which means [`SupervisedWidget`] wraps it
/// unchanged, `Desktop`'s slot arithmetic keeps one list, and the day a
/// built-in is moved out-of-process nothing in `redraw_dock` is
/// touched. The alternative that suggests itself — making the dock
/// generic over the item type — would buy exactly nothing (there is one
/// dock, and it is heterogeneous by definition) and cost every call
/// site a turbofish.
///
/// It is introduced *before* the remote half exists, deliberately.
/// Every site that indexes the column — `item_slots`, `redraw_dock`,
/// the drag, the input routing — has to be read and re-reasoned about
/// when the thing it is indexing changes kind. Doing that once, against
/// a one-variant enum whose behavior is provably identical to what it
/// replaced, is a different and much smaller job than doing it in the
/// same change that introduces sockets, subprocesses and a crash-loop
/// budget.
///
/// # The id is here, not on the trait
///
/// [`DockWidget::name`] is a *display* label — "CLK", "NET" — drawn on
/// the dead-screen tile. It is not an identity: renaming a label is a
/// cosmetic change, and if it doubled as the persistence key it would
/// silently reset every user's dock arrangement. So the id is assigned
/// where an item enters the column, in `Desktop::new`'s list, next to
/// the constructor it names.
pub(crate) enum DockItem {
    /// An instrument compiled into the shell — `chonk-instruments`.
    ///
    /// These stay in-process, and that is a decision rather than an
    /// unfinished migration. The risk a process boundary contains is
    /// not present for code that ships with the compositor; after
    /// declarative sampling they cannot block the loop, so the boundary
    /// buys nothing on the axis that motivated it; and the cost is six
    /// processes, six `FontSystem`s (the sampling work *deleted* five
    /// of those, rather than multiplying them by six), six sockets and
    /// six crash budgets. What matters is that a built-in and a remote
    /// tile are interchangeable *here*, which is what lets any one of
    /// them move out later without touching the dock.
    Builtin { id: &'static str, widget: Box<dyn DockWidget> },
}

impl DockItem {
    /// A built-in instrument under its reserved persistence id.
    ///
    /// `id` is written including [`BUILTIN_PREFIX`] rather than having
    /// it prepended here, so that the literal in `Desktop::new` is
    /// character-for-character what appears in the user's `dock-items`
    /// file. A grep for the line in the file finds the line in the
    /// source. `builtin_ids_are_prefixed_and_unique` keeps the two
    /// honest.
    pub(crate) fn builtin(id: &'static str, widget: Box<dyn DockWidget>) -> Self {
        // The reserved namespace is what stops a `.dockapp` declaring
        // `id = "clock"` from taking the analog clock's line in the
        // user's `dock-items` file. A built-in that forgot the prefix
        // would be the one hole in that, and would not fail anywhere
        // visible — it would simply be displaceable. `debug_assert`
        // rather than a hard panic: this is a mistake made at compile
        // time by whoever adds the seventh instrument, so a developer
        // build is exactly where it should stop.
        debug_assert!(id.starts_with(BUILTIN_PREFIX), "a built-in dock item's id must begin with `{BUILTIN_PREFIX}`, got {id:?}");
        DockItem::Builtin { id, widget }
    }

    /// This item's persistence key — see [`BUILTIN_PREFIX`].
    pub(crate) fn id(&self) -> &str {
        match self {
            DockItem::Builtin { id, .. } => id,
        }
    }
}

/// The whole reason [`DockItem`] is an enum behind this trait rather
/// than something the dock has to match on: every one of these
/// forwards, and the dock never learns which arm it took.
impl DockWidget for DockItem {
    fn name(&self) -> &str {
        match self {
            DockItem::Builtin { widget, .. } => widget.name(),
        }
    }

    fn sources(&self) -> Vec<Source> {
        match self {
            DockItem::Builtin { widget, .. } => widget.sources(),
        }
    }

    fn bind(&mut self, ids: &[SourceId]) {
        match self {
            DockItem::Builtin { widget, .. } => widget.bind(ids),
        }
    }

    fn update(&mut self, samples: &Samples) -> bool {
        match self {
            DockItem::Builtin { widget, .. } => widget.update(samples),
        }
    }

    fn render(&self, theme: &Theme, tile: u32, fonts: &mut cosmic_text::FontSystem, swash: &mut cosmic_text::SwashCache) -> DecorationBuffer {
        match self {
            DockItem::Builtin { widget, .. } => widget.render(theme, tile, fonts, swash),
        }
    }

    fn tile_height(&self) -> u32 {
        match self {
            DockItem::Builtin { widget, .. } => widget.tile_height(),
        }
    }

    fn on_input(&mut self, input: DockInput, tile: u32) -> Vec<Effect> {
        match self {
            DockItem::Builtin { widget, .. } => widget.on_input(input, tile),
        }
    }
}

/// How long one `DockWidget` call may take before the dock names it in
/// the log. Exceeding this is *reported*, not punished — see
/// [`SEVERE_BUDGET`] for the threshold that actually costs a widget its
/// slot.
///
/// Every `update`, `render` and `on_input` in this SDK runs on the
/// compositor's single repaint thread, which wakes on a 16ms
/// housekeeping bound (`HOUSEKEEPING_INTERVAL`, `wm-wayland/src/
/// state.rs`). That 16ms is the *whole* frame's budget — every widget
/// in the stack, plus the dock blit, plus compositing — so half of it
/// spent inside one widget is already an unambiguous fault rather than
/// a slow day.
///
/// Measured, so the number is not taste: at the stock 56px tile on the
/// development machine, a built-in widget's steady-state `render` costs
/// 18–60µs in release and 0.7–1.0ms in a debug build, its first
/// (cache-cold) `render` costs 0.17–0.30ms release / 2.3–3.1ms debug,
/// and `update` costs under 12µs in either. 8ms therefore sits ~130x
/// above the real steady-state cost in release and ~8x in the debug
/// build a developer actually runs, which is the property that matters:
/// a line in the log is evidence of a defect, never of a busy machine.
const BUDGET: Duration = Duration::from_millis(8);

/// How long one call may take before it counts as an offence against
/// [`OFFENCES_BEFORE_EVICTION`].
///
/// The two-tier split is deliberate. Until Layer 3 landed the reason
/// was concrete and current: four of the six built-in instruments still
/// did blocking file I/O on the repaint path even after the `nmcli`
/// fix — sysload read `/proc/stat` and `/proc/meminfo`, net read
/// `/proc/net/dev`, power walked `/sys/class/power_supply` with up to
/// four reads per supply, wifi walked `/sys/class/net` with four probes
/// per interface. Every one of those now runs on a sampler thread (see
/// [`sampling`]), so what remains under this budget is rendering, which
/// on healthy hardware is tens of microseconds and is nonetheless
/// exactly the kind of call that can drift over 8ms on a loaded machine
/// or an unusual font stack. Destroying a working instrument over a 9ms
/// hiccup would leave a worse desktop than the hiccup did, so 8ms buys
/// a log line and nothing more.
///
/// 100ms is where "stuttered" becomes "stopped": six-plus consecutive
/// frames dropped at the 16ms cadence, and the long-standing UI
/// threshold past which an interaction stops reading as instantaneous
/// and starts reading as broken. A widget that does that repeatedly is
/// not having a bad moment, it is the thing wrong with the desktop.
///
/// The case this was written expecting to catch no longer arrives here:
/// `wifi.rs`'s `/sys/class/net/*/speed` read dispatches into the
/// driver's `ethtool` op, which on some NICs blocks for hundreds of
/// milliseconds and does so uninterruptibly, and it used to be issued
/// from `tick()`. It is now a [`Source::Tree`] field read on a sampler
/// thread, where the same stall costs a stretched sampling interval and
/// a tile that updates late. Nothing about this threshold changes for
/// that: it is what still stands between the desktop and whatever the
/// next widget does that nobody modelled, and if it ever does fire the
/// log will say exactly which widget and exactly how long. The user
/// keeps a responsive desktop minus one instrument. That is the trade
/// this mechanism exists to make.
const SEVERE_BUDGET: Duration = Duration::from_millis(100);

/// A single call this long evicts the widget on the spot, without
/// waiting out [`OFFENCES_BEFORE_EVICTION`].
///
/// This threshold is taken from the compositor rather than chosen:
/// `wm-wayland/src/session.rs` logs "no page-flip completion from the
/// DRM device" at 2s (`FLIP_STALL_WARNING`) and *resets the DRM device*
/// at 5s (`FLIP_STALL_RECOVERY`), and that reset is not free — on the
/// hardware this was diagnosed on, a modeset commit can block the
/// caller for as long as 8.5s. So a widget that parks the loop for a
/// full second is already most of the way to making the compositor
/// misdiagnose itself the way it did on 2026-08-29, when a wifi tile
/// shelling out to `nmcli dev wifi` (`--rescan auto`, ~3.6s once every
/// ~34s) was logged, every single time, against an innocent and idle
/// display driver. Finding that took four agents and a trip through DRM
/// internals. Letting a widget that has demonstrated it can do that
/// have two more attempts to reach the watchdog is not a mercy worth
/// extending, so its first such call is its last.
const HARD_BUDGET: Duration = Duration::from_secs(1);

/// How many [`SEVERE_BUDGET`] overruns a widget gets before the dock
/// stops calling it at all.
///
/// Counted cumulatively for the session, never consecutively. The
/// incident this whole mechanism exists to catch fired once every ~34
/// seconds — about one frame in two thousand — so a consecutive
/// counter would have been reset by the ~2000 healthy frames in between
/// and would have evicted precisely nothing, no matter how long the
/// session ran.
///
/// Three: the first overrun is plausibly the machine's (a scheduler
/// hiccup, a major page fault, an unlucky cold cache), the second still
/// might be, and the third says it is the widget's.
const OFFENCES_BEFORE_EVICTION: u32 = 3;

/// Which entry point a timing charge came from. Carried into the log —
/// "slow in `render`" and "slow in `update`" point a reader at
/// completely different code — and used to give each entry point its
/// own independent warm-up pass and its own single report line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CallKind {
    Update,
    Render,
    Input,
}

impl CallKind {
    const COUNT: usize = 3;

    fn index(self) -> usize {
        match self {
            CallKind::Update => 0,
            CallKind::Render => 1,
            CallKind::Input => 2,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            CallKind::Update => "update",
            CallKind::Render => "render",
            CallKind::Input => "on_input",
        }
    }
}

/// Per-entry-point bookkeeping. Both flags exist to keep the log honest
/// rather than loud.
#[derive(Clone, Copy, Default)]
struct EntryPoint {
    /// This entry point has completed at least one call, so its
    /// one-time [`Verdict::WarmUp`] pass is spent.
    warmed: bool,
    /// A sub-severe overrun of this entry point has already been named
    /// in the log, and will not be named again.
    ///
    /// This is `PendingFlip::stall_reported` from
    /// `wm-wayland/src/session.rs`, applied to the same problem: a
    /// condition that persists across many wakeups must produce one
    /// line per incident, not one per wakeup, or readers learn to skip
    /// it — and a log nobody reads is how the original incident stayed
    /// invisible for as long as it did. "This widget's `render` is over
    /// budget" is one incident however many frames it spans. A
    /// [`SEVERE_BUDGET`] overrun is exempt from the suppression because
    /// each of those genuinely is its own event, and there can be at
    /// most [`OFFENCES_BEFORE_EVICTION`] of them before the widget is
    /// gone.
    reported: bool,
}

/// The thresholds a [`SupervisedWidget`] judges against. A struct
/// rather than four constants read directly, purely so the tests can
/// drive the state machine with microsecond budgets instead of
/// sleeping out real seconds — production construction always goes
/// through [`Limits::STOCK`].
#[derive(Clone, Copy)]
struct Limits {
    budget: Duration,
    severe: Duration,
    hard: Duration,
    offences: u32,
}

impl Limits {
    const STOCK: Limits =
        Limits { budget: BUDGET, severe: SEVERE_BUDGET, hard: HARD_BUDGET, offences: OFFENCES_BEFORE_EVICTION };
}

/// What one timed call earned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    /// Inside the budget. The overwhelmingly common case, and the only
    /// one that costs nothing but the `Instant::elapsed`.
    Fine,
    /// Over [`Limits::budget`], but on this entry point's very first
    /// call, which is forgiven once and logged at debug only.
    ///
    /// A widget's first `render` pays one-time costs it never pays
    /// again — cosmic-text shaping caches, the glyph atlas, the first
    /// touch of every page the renderer allocates — measured here at
    /// 2.3–3.1ms against a 0.7–1.0ms steady state in a debug build. On
    /// a machine slower than this one that difference is exactly what
    /// would make *startup* the likeliest source of log lines, which is
    /// the wrong bias for a mechanism whose entire value is that its
    /// output means something. The pass is soft only: it never applies
    /// to [`Limits::severe`] or above, because the failure being
    /// guarded against is perfectly capable of happening on the first
    /// call (the `nmcli` sampler blocked on the very first tick too).
    WarmUp,
    /// Over budget but under [`Limits::severe`]: named in the log once
    /// per entry point, never counted, never fatal.
    Slow,
    /// Over [`Limits::severe`]: counted against
    /// [`Limits::offences`] and logged every time.
    Offence,
    /// Enough offences accumulated, or one call over [`Limits::hard`]:
    /// stop calling this widget.
    Evict,
}

/// The whole eviction policy as one pure function of (how long the call
/// took, how many offences preceded it, has this entry point run
/// before). Split out from [`SupervisedWidget`] so the policy is
/// testable against fabricated durations — a test for the one-second
/// hard limit must not cost a second, and a test for the offence count
/// must not depend on a sleep landing accurately on a loaded CI runner.
fn verdict(elapsed: Duration, offences_before: u32, warmed: bool, limits: Limits) -> Verdict {
    if elapsed >= limits.hard {
        return Verdict::Evict;
    }
    if elapsed >= limits.severe {
        return if offences_before + 1 >= limits.offences { Verdict::Evict } else { Verdict::Offence };
    }
    if elapsed < limits.budget {
        return Verdict::Fine;
    }
    if warmed {
        Verdict::Slow
    } else {
        Verdict::WarmUp
    }
}

/// A dock widget plus the dock's guard against it.
///
/// # Why the dock does not simply trust its widgets
///
/// A `DockWidget` is called from the compositor's repaint loop, which
/// is single-threaded: for as long as any widget is inside `update` or
/// `render`, the desktop draws nothing, reads no input, and does not
/// collect the page-flip completion already sitting in its DRM fd. That
/// is not a hypothetical. It happened, the whole desktop froze in ~3.6s
/// bursts, and the compositor blamed its display driver for it (see
/// [`HARD_BUDGET`] and [`sampling::BackgroundCommand`]).
///
/// So the dock stops taking a widget's word for it and times every call
/// across the trait boundary:
///
/// * over [`BUDGET`], the log names the widget, the entry point and the
///   duration — once, then quietly (see [`EntryPoint::reported`]).
/// * over [`SEVERE_BUDGET`], that becomes a counted offence, logged
///   every time because there can be at most
///   [`OFFENCES_BEFORE_EVICTION`] of them.
/// * past that, or once past [`HARD_BUDGET`] in a single call, the dock
///   evicts it: the widget is never called again and its slot shows a
///   dead-screen tile carrying its [`DockWidget::name`]. A broken
///   widget degrades to a missing instrument, which is a bad afternoon,
///   instead of to a frozen desktop, which is a lost session.
///
/// # This is a backstop, not the boundary
///
/// Worth being precise about, because the difference is the whole
/// lesson of the incident: a watchdog notices a freeze *after* it has
/// happened. A structure in which the freeze cannot happen is different
/// in kind, and it is what actually retires the `nmcli` class of bug.
/// That structure is [`sampling`], and it has landed: widgets declare
/// what they need sampled (command, file, sysfs subtree, interval), the
/// dock owns every sampler thread, and `tick` has become an
/// `update(&Samples)` fold over data that was collected somewhere else
/// — with no path from a widget to a syscall at all. A built-in widget
/// now structurally *cannot* block the loop by sampling, and nothing
/// here will fire for that reason again.
///
/// This layer keeps earning its place on what that does not cover, and
/// the list is not short: a widget's own accidentally quadratic render,
/// a pathological cosmic-text shaping path, a `Drop` that decides to
/// flush something, a third-party dockapp doing something nobody
/// modelled. Sampling was the *known* way to freeze the loop, not the
/// only one, and this type is the part of the answer that does not have
/// to know what the next one will be. Do not read it as the reason the
/// wifi incident cannot recur — that is [`sampling`]'s job now. Read it
/// as the reason the *next* one will name itself in a single log line
/// instead of costing four agents a trip through DRM internals.
///
/// # Eviction is permanent for the session
///
/// Deliberately. There is no way to learn whether a widget has
/// recovered except by calling it again, and calling it again is the
/// exact risk being retired — a widget blocking on a hardware wifi scan
/// "recovers" every 34 seconds and re-freezes the desktop every 34
/// seconds, so a retry policy would have converted this incident into a
/// slower version of itself rather than ending it. Restarting the
/// session is cheap, explicit, and something the user chooses.
///
/// The evicted widget's value is kept rather than dropped: `Drop` is
/// just one more piece of a misbehaving widget's code, and running it
/// on the repaint thread at the exact moment that thread has already
/// proven it cannot afford to wait would be a strange reward for
/// noticing.
///
/// # It covers remote tiles too
///
/// What this wraps is a [`DockItem`], not a built-in widget: an
/// out-of-process dockapp's tile is supervised on exactly the same
/// terms, and arguably needs it more, since its code is not in this
/// repository at all. Nothing here had to become generic for that.
/// `DockItem` implements [`DockWidget`] — its `name` is the item's
/// label, its `render` either the widget's pixels or the remote tile's
/// last delivered frame — so every method below is the same code it
/// was, and the timing charge lands on whichever kind of item was slow.
///
/// Note what that does *not* mean for a remote tile. A dockapp that
/// hangs costs this budget nothing, because the shell never calls into
/// it: it reads a socket that has stopped producing. The eviction
/// machinery here fires on a `DockItem` whose *in-process* work is slow
/// — a pathological render of a stale frame, say — and a hung dockapp
/// is caught by its own liveness check instead. Two mechanisms, two
/// different failures; see `crate::dockapp`.
pub(crate) struct SupervisedWidget {
    item: DockItem,
    /// Cached at construction so the log and the tombstone still have
    /// an identity after eviction, without calling back into the item
    /// for it.
    ///
    /// A `String` rather than a `&'static str`: a remote tile's name
    /// comes from a `.dockapp` file read at startup. See
    /// [`DockWidget::name`] for why the trait relaxed rather than the
    /// dock leaking one literal per registry scan.
    name: String,
    limits: Limits,
    offences: u32,
    entry: [EntryPoint; CallKind::COUNT],
    evicted: bool,
    /// An eviction happened outside `tick` (in `render` or a click) and
    /// the dock has not laid out for it yet: this slot's height just
    /// collapsed to one tile and its face just became a tombstone.
    /// Cleared by the next [`SupervisedWidget::tick`], which reports
    /// "changed" so the dock relays out exactly once.
    relayout: bool,
}

impl SupervisedWidget {
    pub(crate) fn new(item: DockItem) -> Self {
        let name = item.name().to_string();
        Self {
            item,
            name,
            limits: Limits::STOCK,
            offences: 0,
            entry: [EntryPoint::default(); CallKind::COUNT],
            evicted: false,
            relayout: false,
        }
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// This item's persistence key — see [`DockItem::id`]. Answered
    /// even for an evicted item: the arrangement the user chose
    /// outlives one instrument going dark, and rewriting the order file
    /// without it would quietly forget where they had put it.
    pub(crate) fn id(&self) -> &str {
        self.item.id()
    }

    /// An evicted widget occupies exactly one tile whatever it used to
    /// think: its `tile_height` is one more answer from code that has
    /// already been disowned, and a tombstone is a square dead screen.
    pub(crate) fn tile_height(&self) -> u32 {
        if self.evicted {
            1
        } else {
            self.item.tile_height().max(1)
        }
    }

    /// Hands the widget this pass's readings and reports whether the
    /// dock needs to repaint. See [`DockWidget::update`].
    pub(crate) fn update(&mut self, samples: &Samples) -> bool {
        if self.evicted {
            // One last "yes, repaint" so the dock picks up the
            // tombstone and the height it collapsed to, then silence.
            return std::mem::take(&mut self.relayout);
        }
        let start = Instant::now();
        let changed = self.item.update(samples);
        self.charge(CallKind::Update, start.elapsed());
        changed || self.evicted
    }

    /// The sources this widget wants sampled, and the ids they became —
    /// paired here rather than at the two call sites so the dock cannot
    /// register a widget's sources and then forget to tell it about
    /// them. Called once, when the widget enters the dock.
    ///
    /// Not timed: this runs during `Desktop::new`, before there is a
    /// frame to be late for, and the budget machinery's whole meaning is
    /// "you were on the repaint thread".
    pub(crate) fn bind(&mut self, registry: &mut SamplerRegistry) {
        let ids = registry.register(self.item.sources());
        self.item.bind(&ids);
    }

    /// `None` means "evicted — draw the tombstone instead". The buffer
    /// from a call that *became* the evicting offence is still returned
    /// and still drawn: those pixels were paid for and are as valid as
    /// the frame before them, and `relayout` has the next tick swap in
    /// the tombstone one frame later.
    pub(crate) fn render(
        &mut self,
        theme: &Theme,
        tile: u32,
        fonts: &mut cosmic_text::FontSystem,
        swash: &mut cosmic_text::SwashCache,
    ) -> Option<DecorationBuffer> {
        if self.evicted {
            return None;
        }
        let start = Instant::now();
        let buffer = self.item.render(theme, tile, fonts, swash);
        self.charge(CallKind::Render, start.elapsed());
        Some(buffer)
    }

    /// Input is timed on the same terms as `update` and `render`,
    /// because it arrives on the same thread. That a widget can now only
    /// *return* an [`Effect::Run`] rather than execute one is what took
    /// `wpctl set-volume` and `nmcli radio wifi off` off this path; what
    /// is left to time is the widget's own hit-testing and state
    /// change, which is the same class of work as `update`.
    ///
    /// An evicted widget swallows the input and emits nothing — the
    /// tombstone is a dead screen, and dead screens have no controls.
    /// If this call is the one that evicts, the eviction itself is the
    /// repaint: the tombstone has to reach the screen.
    pub(crate) fn on_input(&mut self, input: DockInput, tile: u32) -> Vec<Effect> {
        if self.evicted {
            return Vec::new();
        }
        let start = Instant::now();
        let mut effects = self.item.on_input(input, tile);
        self.charge(CallKind::Input, start.elapsed());
        if self.evicted {
            effects.push(Effect::Repaint);
        }
        effects
    }

    /// Books one timed call against the budget and acts on the verdict.
    fn charge(&mut self, kind: CallKind, elapsed: Duration) {
        let entry = &mut self.entry[kind.index()];
        let warmed = std::mem::replace(&mut entry.warmed, true);
        match verdict(elapsed, self.offences, warmed, self.limits) {
            Verdict::Fine => {}
            Verdict::WarmUp => {
                tracing::debug!(
                    widget = self.name,
                    call = kind.as_str(),
                    ?elapsed,
                    budget = ?self.limits.budget,
                    "dock widget overran its frame budget on its first call; forgiven once as \
                     cache-cold warm-up. If it repeats, it is real and will be logged as such."
                );
            }
            Verdict::Slow => {
                // One line per (widget, entry point) for the whole
                // session — see `EntryPoint::reported`.
                if std::mem::replace(&mut entry.reported, true) {
                    return;
                }
                tracing::error!(
                    widget = self.name,
                    call = kind.as_str(),
                    ?elapsed,
                    budget = ?self.limits.budget,
                    "dock widget overran its frame budget on the compositor's repaint thread, \
                     where the desktop draws nothing and reads no input until it returns. Not \
                     counted against eviction at this size, and this is the only line it will get: \
                     move the slow work to a worker thread (see BackgroundCommand) and let the tile \
                     go stale instead."
                );
            }
            Verdict::Offence => {
                self.offences += 1;
                tracing::error!(
                    widget = self.name,
                    call = kind.as_str(),
                    ?elapsed,
                    severe_budget = ?self.limits.severe,
                    offence = self.offences,
                    of = self.limits.offences,
                    "dock widget stopped the compositor's repaint thread outright; the desktop \
                     drew nothing, read no input and collected no page flip for that long. It is \
                     evicted from the dock on offence {}.",
                    self.limits.offences
                );
            }
            Verdict::Evict => {
                self.offences += 1;
                self.evicted = true;
                self.relayout = true;
                tracing::error!(
                    widget = self.name,
                    call = kind.as_str(),
                    ?elapsed,
                    severe_budget = ?self.limits.severe,
                    hard_budget = ?self.limits.hard,
                    offences = self.offences,
                    "evicting dock widget: it has blocked the compositor's repaint thread often \
                     enough (or once badly enough) that the desktop is better off without it. Its \
                     tile is now a dead screen and it will not be called again this session; \
                     restart the shell once it is fixed."
                );
            }
        }
    }

    #[cfg(test)]
    fn with_limits(item: DockItem, limits: Limits) -> Self {
        Self { limits, ..Self::new(item) }
    }
}

/// The workspace state the Clip tile and the `Desktop` share
/// through one `Rc<RefCell<...>>`: the WM's event loop pushes the
/// authoritative `(current, count)` in through
/// `Desktop::set_workspace_display`, and the Clip's click handler
/// pushes a switch request out through `requested` for the loop to
/// drain via `Desktop::take_workspace_request`. A shared cell instead
/// of widget methods because `Desktop` stores widgets as
/// `Box<dyn DockWidget>` — by design the dock can't reach a specific
/// widget's internals, so state that crosses that boundary travels
/// beside the trait object, not through it.
pub(crate) struct WorkspaceShared {
    pub current: usize,
    pub count: usize,
    /// A workspace index the user clicked their way toward, waiting for
    /// the WM to actually perform the switch — `Some` is a request, not
    /// a fact, which is why the click handler never repaints: the tile
    /// keeps showing the real current workspace until the WM confirms
    /// the switch by updating `current`/`count`.
    pub requested: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chonk_dock_widget::SampleBench;
    use std::cell::Cell;
    use std::rc::Rc;
    use wm_theme_api::Point;

    /// A widget that takes exactly as long as it is told to. The sleep
    /// is real (there is no clock to inject through the `DockWidget`
    /// trait, and inventing one would change the very boundary under
    /// test), so every duration here is kept in the microseconds and
    /// the thresholds are brought down to meet them via
    /// [`SupervisedWidget::with_limits`]. The stock policy — including
    /// the one-second hard limit, which no test may pay for — is
    /// exercised against fabricated durations in [`verdict`]'s own
    /// tests below.
    struct SlowWidget {
        cost: Duration,
        ticks: Rc<Cell<u32>>,
        renders: Rc<Cell<u32>>,
    }

    impl SlowWidget {
        fn new(cost: Duration) -> Self {
            Self { cost, ticks: Rc::new(Cell::new(0)), renders: Rc::new(Cell::new(0)) }
        }
    }

    impl DockWidget for SlowWidget {
        fn name(&self) -> &str {
            "SLOW"
        }

        fn update(&mut self, _samples: &Samples) -> bool {
            self.ticks.set(self.ticks.get() + 1);
            std::thread::sleep(self.cost);
            true
        }

        fn render(&self, _theme: &Theme, _tile: u32, _fonts: &mut cosmic_text::FontSystem, _swash: &mut cosmic_text::SwashCache) -> DecorationBuffer {
            self.renders.set(self.renders.get() + 1);
            std::thread::sleep(self.cost);
            DecorationBuffer { width: 1, height: 1, pixels: vec![0; 4] }
        }

        fn tile_height(&self) -> u32 {
            3
        }
    }

    /// Budgets small enough that a test can overrun them on purpose in
    /// well under a millisecond, in the same proportions as the stock
    /// ones (severe is an order of magnitude over budget; hard is an
    /// order of magnitude over that).
    const TEST_LIMITS: Limits = Limits {
        budget: Duration::from_micros(100),
        severe: Duration::from_micros(500),
        hard: Duration::from_millis(50),
        offences: OFFENCES_BEFORE_EVICTION,
    };

    fn theme() -> Theme {
        wm_theme::default_theme::all_themes().into_iter().next().expect("the theme set is never empty")
    }

    /// The supervision tests care about timing, not readings, so they
    /// drive `update` with an empty `Samples` — which is also the exact
    /// input a widget declaring no sources sees in production.
    fn no_samples() -> SampleBench {
        SampleBench::new()
    }

    #[test]
    fn a_call_inside_the_budget_is_never_charged() {
        assert_eq!(verdict(Duration::from_micros(60), 0, true, Limits::STOCK), Verdict::Fine);
        // The slowest thing measured on the development machine: a
        // debug-build cache-cold render. It must not even be reported,
        // or startup becomes the likeliest source of log lines.
        assert_eq!(verdict(Duration::from_micros(3_100), 0, true, Limits::STOCK), Verdict::Fine);
    }

    #[test]
    fn the_first_call_of_each_kind_is_forgiven_once_but_only_softly() {
        assert_eq!(verdict(Duration::from_millis(9), 0, false, Limits::STOCK), Verdict::WarmUp);
        assert_eq!(verdict(Duration::from_millis(9), 0, true, Limits::STOCK), Verdict::Slow);
        // The warm-up pass buys nothing once a call is actually
        // stopping the screen: the incident being guarded against
        // blocked on its *first* tick too.
        assert_eq!(verdict(SEVERE_BUDGET, 0, false, Limits::STOCK), Verdict::Offence);
        assert_eq!(verdict(HARD_BUDGET, 0, false, Limits::STOCK), Verdict::Evict);
    }

    /// The two tiers do different jobs: being over budget is worth
    /// saying, and only being over the severe budget is worth a slot.
    /// Losing a working instrument to a 9ms hiccup would be a worse
    /// desktop than the hiccup was.
    #[test]
    fn a_merely_over_budget_call_is_reported_but_never_counted() {
        for offences in 0..100 {
            assert_eq!(verdict(Duration::from_millis(9), offences, true, Limits::STOCK), Verdict::Slow);
        }
    }

    #[test]
    fn three_severe_overruns_evict_and_no_fewer() {
        let severe = SEVERE_BUDGET + Duration::from_millis(1);
        assert_eq!(verdict(severe, 0, true, Limits::STOCK), Verdict::Offence);
        assert_eq!(verdict(severe, 1, true, Limits::STOCK), Verdict::Offence);
        assert_eq!(verdict(severe, 2, true, Limits::STOCK), Verdict::Evict);
    }

    /// The regression guard for the actual 2026-08-29 incident shape:
    /// `nmcli dev wifi --rescan auto` blocked the repaint thread for
    /// ~3.6s, once every ~34s. One of those is past the hard limit, so
    /// it costs the desktop one freeze and not a fourth.
    #[test]
    fn one_nmcli_shaped_stall_evicts_immediately() {
        assert_eq!(verdict(Duration::from_millis(3_600), 0, true, Limits::STOCK), Verdict::Evict);
        assert!(
            HARD_BUDGET < Duration::from_secs(2),
            "the hard limit must sit below wm-wayland's FLIP_STALL_WARNING (2s), or a widget can \
             still get the compositor to blame its display driver twice"
        );
    }

    #[test]
    fn an_evicted_widget_is_never_called_again() {
        let bench = no_samples();
        let (mut fonts, mut swash) = (cosmic_text::FontSystem::new(), cosmic_text::SwashCache::new());
        let widget = SlowWidget::new(TEST_LIMITS.severe * 2);
        let (ticks, renders) = (Rc::clone(&widget.ticks), Rc::clone(&widget.renders));
        let mut supervised = SupervisedWidget::with_limits(DockItem::builtin("builtin:test", Box::new(widget)), TEST_LIMITS);

        // Three severe overruns. The warm-up pass does not apply to
        // these, so the third one is the eviction.
        for _ in 0..3 {
            supervised.update(&bench.samples());
        }
        assert!(supervised.evicted, "three severe updates should have evicted it");
        assert_eq!(ticks.get(), 3);

        let before = ticks.get();
        for _ in 0..10 {
            supervised.update(&bench.samples());
        }
        assert_eq!(ticks.get(), before, "an evicted widget must not be updated again");
        assert!(supervised.render(&theme(), 56, &mut fonts, &mut swash).is_none(), "an evicted widget renders no pixels of its own");
        assert_eq!(renders.get(), 0);
        let effects = supervised.on_input(DockInput::Press { local: Point::new(1, 1), button: wm_core::MouseButton::Left }, 56);
        assert!(effects.is_empty(), "a tombstone has no controls");
    }

    #[test]
    fn eviction_collapses_the_slot_to_one_tile_and_asks_for_exactly_one_relayout() {
        let bench = no_samples();
        let (mut fonts, mut swash) = (cosmic_text::FontSystem::new(), cosmic_text::SwashCache::new());
        let mut supervised = SupervisedWidget::with_limits(DockItem::builtin("builtin:test", Box::new(SlowWidget::new(TEST_LIMITS.severe * 2))), TEST_LIMITS);
        assert_eq!(supervised.tile_height(), 3, "a healthy widget keeps whatever height it asked for");

        // Evict through `render`, so the relayout request has to
        // survive until an update can carry it.
        for _ in 0..3 {
            let _ = supervised.render(&theme(), 56, &mut fonts, &mut swash);
        }
        assert!(supervised.evicted);
        assert_eq!(supervised.tile_height(), 1, "a tombstone is one square dead screen");
        assert!(supervised.update(&bench.samples()), "the first update after an eviction must ask the dock to lay out again");
        assert!(!supervised.update(&bench.samples()), "and exactly once — a tombstone never changes after that");
    }

    /// Offences are cumulative across entry points and across the whole
    /// session, because the incident that motivated all of this fired
    /// roughly one frame in two thousand: anything that reset on a good
    /// frame would have caught it never.
    #[test]
    fn offences_accumulate_across_entry_points_and_across_healthy_frames_in_between() {
        let mut supervised = SupervisedWidget::with_limits(DockItem::builtin("builtin:test", Box::new(SlowWidget::new(TEST_LIMITS.severe * 2))), TEST_LIMITS);
        supervised.charge(CallKind::Update, TEST_LIMITS.severe);
        for _ in 0..2_000 {
            supervised.charge(CallKind::Render, Duration::ZERO);
        }
        supervised.charge(CallKind::Render, TEST_LIMITS.severe);
        assert!(!supervised.evicted, "two offences is not three, however far apart they fell");
        supervised.charge(CallKind::Input, TEST_LIMITS.severe);
        assert!(supervised.evicted, "the third offence evicts wherever it came from");
    }

    #[test]
    fn a_healthy_widget_is_never_evicted_however_long_it_runs() {
        struct Cheap;
        impl DockWidget for Cheap {
            fn name(&self) -> &str {
                "OK"
            }
            fn update(&mut self, _samples: &Samples) -> bool {
                false
            }
            fn render(&self, _theme: &Theme, _tile: u32, _fonts: &mut cosmic_text::FontSystem, _swash: &mut cosmic_text::SwashCache) -> DecorationBuffer {
                DecorationBuffer { width: 1, height: 1, pixels: vec![0; 4] }
            }
        }

        let bench = no_samples();
        let mut supervised = SupervisedWidget::new(DockItem::builtin("builtin:cheap", Box::new(Cheap)));
        for _ in 0..10_000 {
            supervised.update(&bench.samples());
        }
        assert!(!supervised.evicted);
        assert_eq!(supervised.offences, 0);
    }

    /// The tombstone path end to end, for the labels that would
    /// actually reach it: `Desktop::redraw_dock` hands a widget's
    /// `name()` straight to `panel::render_dead_tile`, which allocates
    /// and `expect`s a pixmap. A name that made that path panic would
    /// turn an eviction — the graceful degradation — into a crash of
    /// the whole shell, which is the one outcome worse than the freeze
    /// being guarded against.
    #[test]
    fn the_tombstone_face_renders_for_every_built_in_name() {
        let theme = theme();
        let mut font_system = cosmic_text::FontSystem::new();
        let mut swash_cache = cosmic_text::SwashCache::new();
        for name in ["NET", "LOAD", "SND", "LNK", "PWR", "CLK"] {
            let tile = wm_theme::panel::render_dead_tile(&theme, &mut font_system, &mut swash_cache, 56, name);
            assert_eq!((tile.width, tile.height), (56, 56), "{name}'s tombstone must fill its slot");
            assert_eq!(tile.pixels.len(), 56 * 56 * 4);
        }
    }

    /// Every built-in has to answer the "which widget was it?" question
    /// that the incident could not, and answer it in the shape the
    /// dead-screen face can actually draw.
    #[test]
    fn every_built_in_widget_names_itself_shortly_and_distinctly() {
        // Each widget is kept alive in the vector rather than named
        // from a temporary: `DockWidget::name` borrows from `&self` now
        // that a remote tile's name is a `String` it owns.
        let widgets: Vec<Box<dyn DockWidget>> = vec![
            Box::new(NetTrafficWidget::new()),
            Box::new(SysLoadWidget::new()),
            Box::new(SoundWidget::new()),
            Box::new(WifiWidget::new()),
            Box::new(PowerWidget::new()),
            Box::new(ClockWidget::new()),
        ];
        let names: Vec<&str> = widgets.iter().map(|widget| widget.name()).collect();
        for name in &names {
            assert!(!name.is_empty(), "a nameless widget is an unactionable log line");
            assert!(name.len() <= 5, "{name:?} is too long for the tombstone tile's face");
            assert_eq!(*name, name.to_uppercase(), "{name:?} should match the SDK's NET/SND/LNK empty-state labels");
        }
        let unique: std::collections::HashSet<&&str> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "two widgets sharing a name make the log ambiguous: {names:?}");
    }
}

