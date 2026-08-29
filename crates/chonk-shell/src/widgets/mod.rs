//! The dock's widget SDK: one small trait, [`DockWidget`], plus one
//! module per built-in widget. A widget owns whatever animation state
//! it needs and renders itself on demand — the same contract for every
//! widget, so `Desktop`'s dock layout and drag-to-reorder logic never
//! needs to know a widget's internals. Adding a new one is implementing
//! this trait in its own module and pushing it into `Desktop::new`'s
//! widget list; nothing else in the dock changes.
//!
//! The SDK is a three-way split, and each third is somewhere a widget
//! is *not*:
//!
//! * **Sampling** is [`sampling`]. A widget declares [`Source`]s and
//!   reads [`Samples`]; the dock owns every sampler thread. This is
//!   what takes away a widget's *reason* to block the compositor's
//!   repaint thread — which is a thing that happened, cost the desktop
//!   3.6-second freezes, and got blamed on the display driver. See
//!   that module's docs.
//! * **Rendering** is `wm-theme`. Widgets call a pure renderer with
//!   plain values instead of drawing, which is what keeps renderers
//!   unit-testable pixel-for-pixel without a live system, and it is the
//!   pattern third-party `chonk-ui` dockapps should copy.
//!   Instrument-style widgets (screens with LED readouts) build on
//!   `wm_theme::panel`, the theme-reactive glass-and-LED kit.
//! * **Acting** is [`Effect`]. A click that has to run `wpctl` or
//!   `nmcli` returns the intent; the dock runs it off-thread.
//!
//! What is left for a widget is the part only it can do: fold this
//! sample into that state, and say what that state looks like. Both are
//! pure and both are testable against fixtures.
//!
//! Be precise about how strong that is. Nothing in the type system
//! stops someone typing `std::fs::read_to_string` inside an `update`;
//! what has changed is that there is no longer anything for it to buy,
//! because everything a widget needs already arrived as data. Closing
//! the remaining gap is a build-time job rather than a design one:
//! moving these modules into a crate of their own with a `clippy.toml`
//! banning `std::process::Command`, `std::fs::{read, read_to_string,
//! read_dir}` and `std::thread::spawn` makes writing one a compile
//! error. That extraction is the next phase's work; until it lands,
//! this module doc is the rule and [`SupervisedWidget`] is what notices
//! it being broken.

use std::time::{Duration, Instant};

use wm_theme::Theme;
use wm_theme_api::DecorationBuffer;

mod clock;
mod net;
mod power;
pub mod sampling;
mod sound;
mod sysload;
mod wifi;

pub use clock::ClockWidget;
pub use net::NetTrafficWidget;
pub use power::PowerWidget;
pub use sampling::{DockInput, Effect, Samples, Source, SourceId, TreeEntry};
pub use sound::SoundWidget;
pub use sysload::SysLoadWidget;
pub use wifi::WifiWidget;

pub(crate) use sampling::{run_detached, SamplerRegistry};

/// A single dock widget.
///
/// The shape of this trait is the whole of Layer 3: there is no entry
/// point at which a widget has any reason to perform I/O. It declares
/// what it needs ([`sources`](DockWidget::sources)), is told what those
/// turned into ([`bind`](DockWidget::bind)), folds already-collected
/// readings into its own state ([`update`](DockWidget::update)), draws
/// that state ([`render`](DockWidget::render)), and returns intents
/// rather than performing them ([`on_input`](DockWidget::on_input)).
/// Every one of those runs on the compositor's single repaint thread,
/// and not one of them is handed anything to wait on.
pub trait DockWidget {
    /// This widget's identity, for the log and for the tombstone tile
    /// its slot shows if the dock ever has to evict it — see
    /// [`SupervisedWidget`]. Deliberately not defaulted: "some widget
    /// overran its budget" is not a line anyone can act on, and the
    /// only moment the answer is knowable for free is here, in the
    /// widget's own source.
    ///
    /// Keep it short and stable. It is drawn as-is on the dead-screen
    /// face, so it wants the same shape as the empty-state labels the
    /// built-ins already use ("NET", "SND", "LNK") — three or four
    /// upper-case characters, not a sentence.
    fn name(&self) -> &'static str;

    /// Everything this widget needs sampled, declared once at
    /// construction. The dock starts one worker per source and never
    /// calls this again, so it is not a place to react to anything —
    /// which is exactly the property that keeps a widget off the
    /// sampling path entirely. Defaulted empty: a widget that draws
    /// only its own state (a future purely-animated tile) declares
    /// nothing.
    fn sources(&self) -> Vec<Source> {
        Vec::new()
    }

    /// The ids for the sources this widget just declared, in the same
    /// order it declared them. Called once, immediately after
    /// [`sources`](DockWidget::sources), before the first
    /// [`update`](DockWidget::update).
    ///
    /// A widget that declares sources and forgets to implement this
    /// keeps [`SourceId::UNBOUND`] and reads nothing forever, which
    /// shows up as a permanently dead instrument rather than a crash —
    /// see that constant for why that is the deliberate failure mode.
    fn bind(&mut self, ids: &[SourceId]) {
        let _ = ids;
    }

    /// Folds this pass's readings into the widget's state. Returns
    /// whether `render` would now produce different pixels, so the dock
    /// only repaints when something actually changed.
    ///
    /// Called once per event-loop iteration — roughly 60Hz against
    /// sources that update at 1Hz, so the overwhelmingly common case is
    /// that nothing is [`fresh`](Samples::fresh) and this returns
    /// `false` immediately. That asymmetry is why widgets fold on
    /// `fresh` rather than on a clock of their own: "a sampler
    /// completed a run" and "a sixtieth of a second went by" are
    /// different questions, and only the first one is news.
    ///
    /// This replaced `tick()`, and the difference is the point.
    /// `tick()` was a moment at which a widget could do anything,
    /// including read `/proc/stat`, `read_dir` sysfs, or wait on
    /// `nmcli` — all of which four of the six built-ins were still
    /// doing on the repaint thread. `update` is handed data that has
    /// already been collected and has nothing to wait on.
    fn update(&mut self, samples: &Samples) -> bool;

    /// Draws the widget's current state into a tile-sized
    /// premultiplied-RGBA buffer.
    ///
    /// `fonts` and `swash` are the dock's own, threaded through rather
    /// than owned per widget: a `cosmic_text::FontSystem` is a full
    /// fontconfig scan, and the five instruments that each built one at
    /// startup were paying that five times over for the shaping caches
    /// of a handful of three-character labels. `Desktop` already owns
    /// one for its menus and switcher; that is the one every tile now
    /// shapes against, so the caches are shared too.
    ///
    /// `&self` rather than `&mut self` on purpose: rendering must be a
    /// function of state that [`update`](DockWidget::update) already
    /// settled, so that the same state cannot draw two different
    /// things.
    fn render(&self, theme: &Theme, tile: u32, fonts: &mut cosmic_text::FontSystem, swash: &mut cosmic_text::SwashCache) -> DecorationBuffer;

    /// How many `tile`-tall units this widget currently occupies in the
    /// dock's vertical stack. Most widgets are exactly one square tile;
    /// override when a widget's rendered size varies (e.g. by mode).
    fn tile_height(&self) -> u32 {
        1
    }

    /// Pointer input inside this widget's own tile, in that tile's
    /// coordinates (origin at its top-left), with `tile` the tile edge
    /// length — so a widget can carve its face into control zones (a
    /// volume widget's louder/softer halves) without knowing where in
    /// the column the dock put it.
    ///
    /// Returns what it wants done: [`Effect::Repaint`] if its pixels
    /// changed, [`Effect::Run`] for a command, [`Effect::Resample`] to
    /// hurry a sampler. It returns intents rather than acting because a
    /// `wpctl set-volume` or an `nmcli radio wifi off` arrives on the
    /// compositor's repaint thread and can park it exactly as well as a
    /// sample could — the click path was always the sampling problem
    /// wearing a different hat.
    ///
    /// Most widgets have no input behavior; the default no-op covers
    /// them.
    fn on_input(&mut self, input: DockInput, tile: u32) -> Vec<Effect> {
        let _ = (input, tile);
        Vec::new()
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
/// # The shape this wants to grow into
///
/// A later phase puts out-of-process third-party dockapps in the same
/// column, as `enum DockItem { Builtin(Box<dyn DockWidget>),
/// Remote(RemoteTile) }`, and supervision plus tombstoning should cover
/// remote tiles too — arguably more so, since their code is not in this
/// repository at all. Nothing here needs to be generic for that: have
/// `DockItem` implement `DockWidget` (its `name` is the item's
/// identity, its `render` either the widget's pixels or the remote
/// tile's last delivered buffer) and this type wraps it unchanged. The
/// generic parameter that suggests itself today would buy exactly
/// nothing and cost every call site a turbofish.
pub(crate) struct SupervisedWidget {
    widget: Box<dyn DockWidget>,
    /// Cached at construction so the log and the tombstone still have
    /// an identity after eviction, without calling back into the
    /// widget for it.
    name: &'static str,
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
    pub(crate) fn new(widget: Box<dyn DockWidget>) -> Self {
        let name = widget.name();
        Self {
            widget,
            name,
            limits: Limits::STOCK,
            offences: 0,
            entry: [EntryPoint::default(); CallKind::COUNT],
            evicted: false,
            relayout: false,
        }
    }

    pub(crate) fn name(&self) -> &'static str {
        self.name
    }

    /// An evicted widget occupies exactly one tile whatever it used to
    /// think: its `tile_height` is one more answer from code that has
    /// already been disowned, and a tombstone is a square dead screen.
    pub(crate) fn tile_height(&self) -> u32 {
        if self.evicted {
            1
        } else {
            self.widget.tile_height().max(1)
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
        let changed = self.widget.update(samples);
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
        let ids = registry.register(self.widget.sources());
        self.widget.bind(&ids);
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
        let buffer = self.widget.render(theme, tile, fonts, swash);
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
        let mut effects = self.widget.on_input(input, tile);
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
    fn with_limits(widget: Box<dyn DockWidget>, limits: Limits) -> Self {
        Self { limits, ..Self::new(widget) }
    }
}

/// How often the dock re-reads the system on a widget's behalf: the
/// stock [`Source`] interval, shared so the instruments that have no
/// reason to differ visibly agree.
///
/// One second is a reading rate, not a frame rate. `update` is called
/// roughly sixty times a second and folds nothing on fifty-nine of
/// them; that asymmetry is the normal, healthy state of a dock tile
/// and is why [`Samples::fresh`] exists.
pub const SAMPLE_INTERVAL: Duration = Duration::from_millis(1000);

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
    use crate::widgets::sampling::SampleBench;
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
        fn name(&self) -> &'static str {
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
        let mut supervised = SupervisedWidget::with_limits(Box::new(widget), TEST_LIMITS);

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
        let mut supervised = SupervisedWidget::with_limits(Box::new(SlowWidget::new(TEST_LIMITS.severe * 2)), TEST_LIMITS);
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
        let mut supervised = SupervisedWidget::with_limits(Box::new(SlowWidget::new(TEST_LIMITS.severe * 2)), TEST_LIMITS);
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
            fn name(&self) -> &'static str {
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
        let mut supervised = SupervisedWidget::new(Box::new(Cheap));
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
        let names: Vec<&'static str> = vec![
            NetTrafficWidget::new().name(),
            SysLoadWidget::new().name(),
            SoundWidget::new().name(),
            WifiWidget::new().name(),
            PowerWidget::new().name(),
            ClockWidget::new().name(),
        ];
        for name in &names {
            assert!(!name.is_empty(), "a nameless widget is an unactionable log line");
            assert!(name.len() <= 5, "{name:?} is too long for the tombstone tile's face");
            assert_eq!(*name, name.to_uppercase(), "{name:?} should match the SDK's NET/SND/LNK empty-state labels");
        }
        let unique: std::collections::HashSet<&&str> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "two widgets sharing a name make the log ambiguous: {names:?}");
    }
}
