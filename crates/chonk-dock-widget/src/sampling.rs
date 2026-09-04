//! Declarative sampling: the half of the widget SDK that exists so a
//! dock widget cannot reach a syscall.
//!
//! # The rule this module enforces
//!
//! A widget says *what* it needs — this command, that file, this sysfs
//! subtree, at this interval — and never *when* or *on which thread*.
//! The dock owns every sampler thread; [`crate::DockWidget::update`] is handed
//! a [`Samples`] of readings that have already been collected and is a
//! pure fold over them. A widget never gets to say "now", so there is
//! nothing left that a `read_to_string`, a `read_dir` or a
//! `Command::output` written inside a widget would buy it.
//!
//! # Why the sampler *runtime* is not in this crate
//!
//! This module is the vocabulary — [`Source`], [`SourceId`],
//! [`Samples`], [`Effect`], [`DockInput`] — and nothing else. Every
//! thread, every `read_dir`, every `Command` that turns a declared
//! `Source` into a `Samples` lives in `chonk-shell`'s
//! `widgets::sampling`, on the dock's side of the boundary.
//!
//! That split is the whole point of this crate existing at all.
//! `chonk-instruments` — the built-in tiles — depends on this crate
//! and on nothing that can perform I/O, and carries a `clippy.toml`
//! that makes `std::fs::File`, `std::process::Command`,
//! `std::fs::read_to_string`, `std::fs::read`, `std::fs::read_dir` and
//! `std::thread::spawn` build errors inside it. If the registry lived
//! here, an instrument would link the very machinery the lint exists to
//! keep out of reach, and "a widget cannot do I/O" would be back to
//! being a convention. It is now a property of the crate graph.
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
//! could write one. `chonk-shell`'s `SupervisedWidget` still times
//! every call — it covers what this cannot (a quadratic render, a
//! pathological text-shaping path, an out-of-process tile) — but it
//! notices a freeze afterwards, where this makes the freeze
//! structurally unavailable.
//!
//! # What moved onto worker threads
//!
//! Every one of these was, until this vocabulary landed, executed on
//! the repaint thread:
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
//! * `wpctl`, `nmcli` (sound, wifi) — [`Source::Command`], off-thread
//!   via the shell's `BackgroundCommand`, which that variant keeps as
//!   its backend rather than reimplementing.
//!
//! # Deliberately built-in only
//!
//! [`Source::Command`] is arbitrary-argv-by-declaration. The dock
//! executing an argv on a third party's behalf would blur exactly the
//! accountability line the out-of-process dockapp protocol
//! (`chonk-dock-proto`) is drawn to establish, so this SDK is for
//! built-in widgets and stays that way; a dockapp runs its own process
//! and does its own sampling, which is the whole point of putting it in
//! one.

use std::path::PathBuf;
use std::time::Duration;

use wm_core::MouseButton;
use wm_theme_api::Point;


/// A sampling request. Widgets return these from
/// [`DockWidget::sources`](crate::DockWidget::sources) at construction
/// and never see them again; the dock turns each one into a worker and
/// hands back the [`SourceId`] to read it by.
///
/// The interval is a request, not a guarantee. A sampler that takes
/// longer than its interval to complete one run simply runs less often
/// — the alternative (overlapping runs) would let a wedged `nmcli` or a
/// blocking `ethtool` op accumulate threads without bound.
pub enum Source {
    /// An external program, run to completion and its stdout kept.
    /// Backed by the shell's `BackgroundCommand`.
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
/// [`DockWidget::bind`](crate::DockWidget::bind), in the same order it
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

    /// The id for the `index`-th slot of a snapshot.
    ///
    /// Minted by the dock's sampler registry, which lives in
    /// `chonk-shell` — see the module docs for why the runtime is on
    /// the other side of this crate boundary. A widget never calls
    /// this; it is handed its ids through
    /// [`DockWidget::bind`](crate::DockWidget::bind).
    pub fn from_index(index: usize) -> SourceId {
        SourceId(index as u32)
    }

    /// Where in the snapshot this id points, or `None` for
    /// [`SourceId::UNBOUND`].
    pub fn index(self) -> Option<usize> {
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
/// Returned from [`DockWidget::on_input`](crate::DockWidget::on_input)
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
    ///
    /// Read the two fields as the sentence they are: **the program is
    /// compile-time (`&'static str`) and the arguments are runtime
    /// (`Vec<String>`)**. The program is the whitelist — the set of
    /// binaries a built-in widget can run is the set of literals in
    /// this repository, and nothing at runtime can add to it. The
    /// arguments are *allowed* to carry runtime values, because a
    /// control that could not name a sink, a UUID or an SSID would be
    /// useless.
    ///
    /// **Build this with [`Argv`] whenever any word of it comes from
    /// the running system** — a sink name, a connection UUID, an SSID,
    /// a MAC. The literal form is for an argv that is entirely written
    /// out in the source; [`Argv`] is what admits a runtime value, and
    /// what refuses the ones that would turn an operand into an option.
    /// See [`Argv`] for the whole rule.
    Run { program: &'static str, args: Vec<String>, then: Option<SourceId> },
    /// Ask a sampler to sample now. For the case with no command in
    /// front of it.
    Resample(SourceId),
}

/// An argv built shape-first: a compile-time program, compile-time
/// words, and runtime values only through a validated slot.
///
/// # The rule
///
/// **The program and the argv's *shape* are compile-time; a runtime
/// value may only ever be one whole operand, and only through
/// [`value`](Argv::value) or [`number`](Argv::number), which validate
/// it. A value that fails validation does not produce a clipped
/// command — it produces no command at all.**
///
/// # Why there is a rule at all
///
/// [`Effect::Run`]'s safety story has always been "the compiler is the
/// whitelist": the program name is a `&'static str`, so the set of
/// binaries this desktop's own widgets can run is the set written in
/// its source, reviewable by reading it. Panels are what put pressure
/// on that. `wpctl set-volume` needs no arguments a human did not
/// type, but "switch to *this* sink", "bring *this* connection up",
/// "join *this* network" all need a word that came from the system a
/// moment ago — a `pactl` sink name, an `nmcli` UUID, an SSID
/// broadcast by whatever access point is in range.
///
/// Those words are not attacker-chosen in any deep sense, but the last
/// one is broadcast by a stranger, and none of them are *reviewable*:
/// no amount of reading the source tells you what will be in them. So
/// the vocabulary keeps everything else fixed and admits them one
/// operand at a time.
///
/// What the validation refuses, and why it is exactly this list:
///
/// * **A leading `-`.** This is the one that matters. Nothing here
///   goes through a shell — [`Effect::Run`] is `Command::new(program)`
///   with an argv vector, so there is no quoting, no globbing and no
///   metacharacter to escape — but every one of these programs parses
///   its own options, and an SSID named `--terminate` handed to
///   `nmcli` as an operand is not an operand any more. A runtime word
///   is an *operand*, and an operand that looks like an option is
///   refused rather than smuggled.
/// * **Control characters** (including NUL and newline). A NUL cannot
///   survive the trip to `execve` anyway; a newline cannot appear in
///   anything this is for, and can appear in a log line, a `.desktop`
///   file, or another program's parser.
/// * **Empty**, and **anything longer than [`Argv::MAX_VALUE`]**. An
///   SSID is at most 32 bytes and a UUID 36; a kilobyte of "sink name"
///   is a bug upstream, and passing it on turns that bug into this
///   desktop's.
///
/// Spaces, `%`, quotes and UTF-8 are all *fine* and deliberately
/// allowed: `pactl` sink names and SSIDs contain them routinely, and
/// with no shell in the path they are ordinary bytes.
///
/// # Using it
///
/// ```
/// # use chonk_dock_widget::sampling::{Argv, Effect, SourceId};
/// # fn example(uuid: String, confirm: SourceId) -> Option<Effect> {
/// Argv::new("nmcli").word("connection").word("up").value(&uuid).effect(Some(confirm))
/// # }
/// ```
///
/// [`effect`](Argv::effect) answers `None` when any value was refused,
/// so a rejected action is simply an action that did not happen — the
/// panel repaints, the sampler reports what is actually true, and
/// nothing half-formed reaches a process. Widgets in this SDK have no
/// way to report an error to the user and should not grow one for
/// this: the honest feedback is the next reading.
pub struct Argv {
    program: &'static str,
    args: Vec<String>,
    /// Why this argv is dead, if it is — kept rather than returned
    /// per-call so a builder chain stays a chain, and so the reason can
    /// be logged (or asserted on in a test) at the end of it.
    rejected: Option<&'static str>,
}

impl Argv {
    /// The longest a runtime value may be, in bytes. Comfortably above
    /// every identifier these commands actually take (SSID 32, UUID
    /// 36, PipeWire node names well under 100) and far below anything
    /// that could be an accident worth passing on.
    pub const MAX_VALUE: usize = 256;

    /// Starts an argv for `program` — the same compile-time program
    /// name [`Effect::Run`] has always taken, and the reason the set of
    /// binaries a built-in widget can run is readable from the source.
    pub fn new(program: &'static str) -> Argv {
        Argv { program, args: Vec::new(), rejected: None }
    }

    /// One compile-time word: a subcommand (`connection`), a flag
    /// (`--rescan`), a fixed operand (`@DEFAULT_AUDIO_SINK@`, `5%+`).
    /// Never validated, because it is in the source, where a reviewer
    /// can see it — that is the whole distinction this type draws.
    #[must_use]
    pub fn word(mut self, word: &'static str) -> Argv {
        self.args.push(word.to_string());
        self
    }

    /// One runtime operand — the sink name, the UUID, the SSID.
    /// Validated against the rule in the type's docs; a value that
    /// fails kills the whole argv rather than the one word, because a
    /// command missing an operand is a command with a different
    /// meaning (`nmcli connection up` with no UUID is not a smaller
    /// version of the request, it is a usage error at best).
    #[must_use]
    pub fn value(mut self, value: impl AsRef<str>) -> Argv {
        let value = value.as_ref();
        let refusal = if value.is_empty() {
            Some("an empty runtime value")
        } else if value.len() > Self::MAX_VALUE {
            Some("a runtime value longer than Argv::MAX_VALUE")
        } else if value.starts_with('-') {
            Some("a runtime value that starts with '-', which the program would read as an option")
        } else if value.chars().any(|c| c.is_control()) {
            Some("a runtime value containing a control character")
        } else {
            None
        };
        match refusal {
            Some(reason) => self.rejected = self.rejected.or(Some(reason)),
            None => self.args.push(value.to_string()),
        }
        self
    }

    /// One runtime number — a PulseAudio stream index, a percentage.
    /// Always valid, and unsigned on purpose: a negative number
    /// renders with the leading `-` that [`value`](Argv::value) exists
    /// to refuse. A word that genuinely starts with a dash is a flag,
    /// and a flag is compile-time — [`word`](Argv::word).
    #[must_use]
    pub fn number(mut self, value: u64) -> Argv {
        self.args.push(value.to_string());
        self
    }

    /// Why this argv was refused, if it was — for a test that wants to
    /// assert the rule rather than merely observe a `None`.
    pub fn rejected(&self) -> Option<&'static str> {
        self.rejected
    }

    /// The effect to return from
    /// [`on_input`](crate::DockWidget::on_input) or
    /// [`panel_input`](crate::DockWidget::panel_input), or `None` if
    /// any runtime value was refused.
    ///
    /// `then` is [`Effect::Run`]'s resample: the sampler whose next
    /// reading is the authority on what the command did.
    pub fn effect(self, then: Option<SourceId>) -> Option<Effect> {
        if let Some(reason) = self.rejected {
            tracing::warn!(program = self.program, reason, "refusing a widget argv");
            return None;
        }
        Some(Effect::Run { program: self.program, args: self.args, then })
    }
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
    pub fn translated(self, origin: Point) -> DockInput {
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
///
/// Public because the sampler runtime that fills these in lives in
/// `chonk-shell` (see the module docs) and a [`Samples`] borrows a
/// slice of them. It is the seam between the two halves of the SDK, not
/// something a widget ever names: a widget reads a [`Samples`], which
/// is what these accessors are for.
#[derive(Debug, Default)]
pub struct Slot {
    pub reading: Reading,
    pub unusable: bool,
    pub fresh: bool,
}

/// The last thing a sampler produced. Retained across passes: a widget
/// that folds only on `fresh` still wants the value there on the passes
/// in between (`power` compares against it, `net` renders from it), and
/// keeping it is what lets the registry clone out of a sampler's mutex
/// only on the pass its generation actually moved.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Reading {
    #[default]
    Missing,
    Text(String),
    Tree(Vec<TreeEntry>),
    Clock(u32, u32, u32),
}

impl<'a> Samples<'a> {
    /// Wraps the registry's snapshot. The dock calls this once per
    /// pass; a widget is handed the result.
    pub fn from_slots(snapshot: &'a [Slot]) -> Samples<'a> {
        Samples { snapshot }
    }
}

/// Hand-built [`Samples`] for widget tests.
///
/// The entire value of Layer 3 to a test is that a widget's input is
/// now data: `update` over a fixed `Samples` is a pure function, so
/// every widget's fold can be exercised against synthetic `/proc`
/// contents and synthetic sysfs trees with no kernel, no `nmcli`, and
/// no clock. This is the thing that makes that convenient.
///
/// Not `#[cfg(test)]`: `cfg(test)` is per-crate, and every consumer of
/// this fixture — all six instruments in `chonk-instruments`, plus
/// `chonk-shell`'s supervision tests — is a *different* crate, which
/// would see nothing. It compiles to a few dozen bytes of unreferenced
/// code in a release build and buys every widget test a pure
/// `update(&Samples)` with no kernel behind it.
pub struct SampleBench {
    snapshot: Vec<Slot>,
}

impl SampleBench {
    /// Deliberately no `Default`: this is a fixture builder whose
    /// contents are positional (ids are assigned in call order), and
    /// `..Default::default()` on one of those is a mistake waiting for
    /// somewhere to happen.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self { snapshot: Vec::new() }
    }

    fn push(&mut self, reading: Reading) -> SourceId {
        self.snapshot.push(Slot { reading, unusable: false, fresh: true });
        SourceId((self.snapshot.len() - 1) as u32)
    }

    /// A command/file source holding `contents`, marked fresh.
    pub fn text(&mut self, contents: &str) -> SourceId {
        self.push(Reading::Text(contents.to_string()))
    }

    /// A command/file source that has produced nothing.
    pub fn missing(&mut self) -> SourceId {
        self.push(Reading::Missing)
    }

    /// A command source whose program could not be spawned.
    pub fn unusable(&mut self) -> SourceId {
        let id = self.push(Reading::Missing);
        self.snapshot[id.0 as usize].unusable = true;
        id
    }

    pub fn tree(&mut self, entries: Vec<TreeEntry>) -> SourceId {
        self.push(Reading::Tree(entries))
    }

    pub fn clock(&mut self, hms: (u32, u32, u32)) -> SourceId {
        self.push(Reading::Clock(hms.0, hms.1, hms.2))
    }

    /// Replaces a source's reading and marks it fresh — one more
    /// sampler run, which is what a widget folds on.
    pub fn set_text(&mut self, id: SourceId, contents: &str) {
        self.snapshot[id.0 as usize] = Slot { reading: Reading::Text(contents.to_string()), unusable: false, fresh: true };
    }

    pub fn set_tree(&mut self, id: SourceId, entries: Vec<TreeEntry>) {
        self.snapshot[id.0 as usize] = Slot { reading: Reading::Tree(entries), unusable: false, fresh: true };
    }

    pub fn set_clock(&mut self, id: SourceId, hms: (u32, u32, u32)) {
        self.snapshot[id.0 as usize] = Slot { reading: Reading::Clock(hms.0, hms.1, hms.2), unusable: false, fresh: true };
    }

    /// Marks every source stale: the pass where no sampler completed a
    /// run, which is the overwhelmingly common one at 60Hz against a
    /// 1Hz source and the one a widget must fold to "nothing changed".
    pub fn all_stale(&mut self) {
        for slot in &mut self.snapshot {
            slot.fresh = false;
        }
    }

    pub fn samples(&self) -> Samples<'_> {
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

    /// What an `Argv` is *for*: the shape stays in the source and the
    /// runtime word rides in one slot, whole.
    #[test]
    fn an_argv_keeps_its_shape_and_carries_runtime_operands_whole() {
        let mut bench = SampleBench::new();
        let confirm = bench.text("");
        // A name with a space and a `%` in it — both perfectly ordinary
        // in PipeWire node names, and both harmless with no shell in
        // the path.
        let effect = Argv::new("pactl")
            .word("move-sink-input")
            .number(42)
            .value("alsa_output.pci-0000_00_1f.3 [100%]")
            .effect(Some(confirm))
            .expect("nothing here is refusable");
        match effect {
            Effect::Run { program, args, then } => {
                assert_eq!(program, "pactl");
                assert_eq!(args, ["move-sink-input", "42", "alsa_output.pci-0000_00_1f.3 [100%]"]);
                assert_eq!(then, Some(confirm));
            }
            _ => panic!("an argv builds a Run and nothing else"),
        }
    }

    /// The rule, asserted one refusal at a time. Every one of these
    /// kills the *whole* command: a widget action either happens as
    /// written or does not happen, never partially.
    #[test]
    fn a_refused_runtime_value_produces_no_command_at_all() {
        let ssid_shaped_like_an_option = Argv::new("nmcli").word("dev").word("wifi").word("connect").value("--terminate");
        assert_eq!(
            ssid_shaped_like_an_option.rejected(),
            Some("a runtime value that starts with '-', which the program would read as an option")
        );
        assert!(ssid_shaped_like_an_option.effect(None).is_none(), "and it does not run as a shorter command");

        assert!(Argv::new("nmcli").word("connection").word("up").value("").effect(None).is_none());
        assert!(Argv::new("pactl").word("set-default-sink").value("sink\nname").effect(None).is_none());
        assert!(Argv::new("pactl").word("set-default-sink").value("sink\0name").effect(None).is_none());
        assert!(Argv::new("pactl").word("set-default-sink").value("x".repeat(Argv::MAX_VALUE + 1)).effect(None).is_none());
        assert!(Argv::new("pactl").word("set-default-sink").value("x".repeat(Argv::MAX_VALUE)).effect(None).is_some(), "the cap is inclusive");

        // A *compile-time* word is never validated — that is the whole
        // distinction. A flag is a flag because it is in the source.
        assert!(Argv::new("nmcli").word("dev").word("wifi").word("--rescan").word("yes").effect(None).is_some());

        // The first refusal is the reported one, and later good values
        // do not resurrect the argv.
        let dead = Argv::new("nmcli").value("-x").value("fine");
        assert!(dead.rejected().is_some());
        assert!(dead.effect(None).is_none());
    }

    #[test]
    fn unusable_is_reported_and_carries_no_reading() {
        let mut bench = SampleBench::new();
        let id = bench.unusable();
        let samples = bench.samples();
        assert!(samples.unusable(id));
        assert_eq!(samples.text(id), None);
    }
}
