//! The sampler runtime: the dock's half of declarative sampling.
//!
//! A widget declares [`Source`]s and reads [`Samples`]; *this* is where
//! the threads, the `read_dir`s, the `Command`s and the `waitpid`s
//! actually live. The vocabulary is `chonk-dock-widget`; the capability
//! is here.
//!
//! That the two are separate crates is the enforcement, not tidiness.
//! `chonk-instruments` depends on the vocabulary and cannot see this
//! module at all — the dependency edge points from `chonk-shell` to the
//! instruments, so it cannot point back — and its own `clippy.toml`
//! makes `std::fs::File`, `std::process::Command`, `std::fs::{read,
//! read_to_string, read_dir}` and `std::thread::spawn` build errors
//! inside it. Everything this module does, an instrument is structurally
//! unable to do.
//!
//! # Why any of this exists
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
//! Every one of the reads below used to happen on that thread:
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
//! * `wpctl`, `nmcli` (sound, wifi) — [`Source::Command`], via
//!   `BackgroundCommand`.
//!
//! # Built-in only, deliberately
//!
//! [`Source::Command`] is arbitrary-argv-by-declaration. The dock
//! executing an argv on a third party's behalf would blur exactly the
//! accountability line the out-of-process dockapp protocol is drawn to
//! establish, so this registry serves built-in widgets and stays that
//! way; a dockapp runs its own process and does its own sampling, which
//! is the whole point of putting it in one.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chonk_dock_widget::{Reading, Samples, Slot, Source, SourceId, TreeEntry};

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
                SourceId::from_index(self.samplers.len() - 1)
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
        Samples::from_slots(&self.snapshot)
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
            // a wait can park here is this worker. That is exactly
            // the property `clippy.toml`'s ban on `Command::output`
            // exists to force someone to state out loud — see
            // `super::SupervisedWidget` for what happens to a widget
            // that gets it wrong and blocks the repaint thread instead.
            //
            // "Only this worker", though, used to mean *forever*: a
            // bare `output()` on a program that hangs instead of
            // exiting wedges this thread for the life of the session,
            // and the source behind it then shows its last good reading
            // as if it were current. `bluetoothctl` with no `org.bluez`
            // on the bus does exactly that — blocks indefinitely,
            // silently — which is why the deadline below is not a
            // nicety. Stdout is piped so it can be collected after the
            // wait. Stderr is discarded, and that is a deliberate
            // change from "wherever the shell's goes": a sampler runs
            // on a timer forever, so a command that complains on every
            // run does not report a problem — it floods the session
            // log until real errors are unfindable in it. The
            // bluetooth instrument's `busctl` on a machine with no
            // bluetooth daemon wrote "Could not activate remote peer
            // 'org.bluez'" every few seconds and buried a live
            // fullscreen investigation. A sampler's signal is its exit
            // status and its stdout; both are read, and a failed run
            // already clears the tile to its dead face, which is the
            // honest report.
            #[allow(clippy::disallowed_methods)]
            let spawned = Command::new(program)
                .args(&args)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn();
            match spawned {
                // A failed run clears the reading rather than leaving
                // the last good one on screen: a tile showing a network
                // that went away is worse than a tile admitting it does
                // not know. A run killed at the deadline is a failed
                // run by that same rule — the widget draws its dead
                // face rather than a stale number.
                Ok(child) => Outcome::Sampled(wait_with_deadline(child, program, SAMPLE_DEADLINE)),
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

/// Runs a widget's commands **in the order it listed them**, one after
/// another, on a single thread of the dock's own — the executor half
/// of [`chonk_dock_widget::Effect::Run`], one command or several
/// (`PanelReaction::RunAll`).
///
/// This is the click path's version of the whole argument: `wpctl
/// set-volume` and `nmcli radio wifi off` arrive on the same repaint
/// thread a sample would have, and are just as able to park it. The
/// dock hands them here and returns.
///
/// Sequential rather than one thread per command, because the plural
/// exists for actions whose parts are a sequence: switching the
/// default audio sink is `pactl set-default-sink` and then one
/// `pactl move-sink-input` per playing stream, and a widget that lists
/// them in that order means them in that order. One thread also costs
/// less than N and cannot interleave two commands against the same
/// daemon.
///
/// Each command's `then` resample fires as soon as *that* command
/// exits, not at the end of the run: a resample is "the reading you
/// need is ready now", and holding the first one until the last
/// migration finished would make the panel look slower than the
/// system it is reporting on.
///
/// `env` is the desktop's launch environment (`shell::launch_env`),
/// given to every command: an `Effect::Run` that opens a *window* —
/// the wifi join dialog, the Bluetooth pairing dialog — must wear the
/// theme, appearance and scale the desk is wearing, and it learns them
/// the same way every other GUI the shell starts does. A command that
/// draws nothing is unharmed by carrying them.
///
/// Every command runs under [`RUN_DEADLINE`]: a program that hangs
/// instead of exiting is killed rather than pinning this thread (and
/// its child) for the life of the session. `bluetoothctl` with no
/// `org.bluez` on the bus is the case that made this non-hypothetical
/// — it blocks forever, silently — and Omarchy's own scripts wrap
/// every such call in `timeout 2s` for the same reason.
pub(crate) fn run_detached(commands: Vec<(&'static str, Vec<String>, Option<Resampler>)>, env: Vec<(String, String)>) {
    let Some((first, _, _)) = commands.first() else {
        return;
    };
    let name = format!("chonkstep-run-{first}");
    std::thread::Builder::new()
        .name(name)
        .spawn(move || {
            for (program, args, then) in commands {
                // Audited exception to `clippy.toml`'s ban, and the one
                // worth reading twice: `nmcli` reaches this line, and
                // `nmcli` is the exact binary whose blocking call froze
                // the desktop on 2026-08-29. It is safe here for one
                // reason only — this closure is the body of this
                // effect's own worker thread, never the compositor's
                // repaint loop. The widget that asked for it returned
                // an `Effect` and cannot have run anything itself.
                //
                // Output goes nowhere, as it did when this was an
                // `output()` that captured and dropped both streams: an
                // effect's answer is the next *sample*, never its
                // chatter. Discarding rather than piping also means
                // there is no pipe to fill, so a noisy command cannot
                // wedge on a full buffer while this thread waits.
                #[allow(clippy::disallowed_methods)]
                let child = Command::new(program)
                    .args(&args)
                    .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
                match child {
                    Ok(child) => {
                        if wait_with_deadline(child, program, RUN_DEADLINE).is_none() {
                            tracing::warn!(program, "effect command exceeded its deadline and was killed");
                        }
                    }
                    Err(error) => tracing::warn!(?error, program, "effect command could not be started"),
                }
                if let Some(resampler) = then {
                    resampler.resample_soon();
                }
            }
        })
        .map_err(|error| tracing::warn!(?error, "could not start the effect thread; the commands will not run"))
        .ok();
}

/// How long any one command a widget asks for may take before the dock
/// kills it.
///
/// Generous enough for the slow-but-honest ones this desktop actually
/// runs — `nmcli dev wifi connect` negotiates with an access point,
/// and the wifi join dialog is a *window* the user types a passphrase
/// into — and finite, which is the whole point: the failure being
/// prevented is a worker parked forever, not a worker parked a while.
const RUN_DEADLINE: Duration = Duration::from_secs(120);

/// How long a *sampler's* command may take before it is killed and the
/// reading reported as absent.
///
/// Much tighter than [`RUN_DEADLINE`], because a sample is a poll on a
/// timer: anything that has not answered in this long has already
/// missed its interval, and the widget is better told "no reading"
/// (which it draws as a dead face) than left showing a number from
/// before the tool wedged.
const SAMPLE_DEADLINE: Duration = Duration::from_secs(8);

/// Waits for `child` up to `deadline`, killing it if it overruns.
/// `Some(stdout)` for a command that exited successfully within the
/// deadline; `None` for every other outcome — non-zero exit, unreadable
/// output, or the deadline.
///
/// Polling rather than a `wait_timeout` from a crate: this is a worker
/// thread with nothing else to do, the poll is a `waitpid(WNOHANG)`,
/// and 20ms of latency on a command that takes hundreds of
/// milliseconds is invisible. The alternative — a dependency whose job
/// is one loop — is not worth it here.
///
/// One caveat the callers are built around: stdout is read *after* the
/// child exits, so a command that writes more than a pipe buffer's
/// worth (64KB) without exiting would block on the pipe and be killed
/// at the deadline. Every sampler command here is a line-oriented
/// status query measured in kilobytes; a source that needs to stream
/// wants a different mechanism, not a bigger buffer.
fn wait_with_deadline(mut child: std::process::Child, program: &str, deadline: Duration) -> Option<String> {
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                // The child's stdout is a pipe only when the caller
                // asked for one; a `spawn`ed command with inherited
                // stdio has nothing to read and answers with an empty
                // string, which its caller ignores.
                let mut out = String::new();
                if let Some(mut pipe) = child.stdout.take() {
                    use std::io::Read;
                    if pipe.read_to_string(&mut out).is_err() {
                        return None;
                    }
                }
                return Some(out);
            }
            Ok(None) => {
                if start.elapsed() >= deadline {
                    // Kill, then reap: a killed child left unwaited is
                    // a zombie, and this desktop runs for weeks.
                    //
                    // Audited exception to `clippy.toml`'s ban on
                    // `Child::wait`, on both counts the ban names. The
                    // thread: this function only ever runs on a dock
                    // worker (a sampler's, or an effect's), never the
                    // repaint thread — the whole point of the deadline
                    // above is that this worker gets *unstuck*, so
                    // trading the wait for a leaked zombie would be
                    // undoing the fix. The duration: the child has just
                    // been sent SIGKILL, which is not catchable, so
                    // this is a reap of a process already on its way
                    // out rather than a wait on one still working.
                    let _ = child.kill();
                    #[allow(clippy::disallowed_methods)]
                    let _ = child.wait();
                    tracing::warn!(program, ?deadline, "killing a command that never exited");
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => {
                tracing::warn!(?error, program, "could not wait on a command; giving up on it");
                return None;
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(first, vec![SourceId::from_index(0), SourceId::from_index(1)]);
        assert_eq!(second, vec![SourceId::from_index(2)]);
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
    /// The bound is one 60 Hz display frame (16 ms) for
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
    /// that stops drawing. And the same for the plural — a
    /// multi-command action (a sink switch plus its stream migrations)
    /// is one handoff and one thread: the commands wait for each other,
    /// the desktop waits for none of them.
    #[test]
    fn running_effects_returns_before_the_commands_do() {
        let start = std::time::Instant::now();
        run_detached(vec![("sleep", vec!["2".to_string()], None)], Vec::new());
        assert!(start.elapsed() < Duration::from_millis(50), "run_detached must not wait on the child");

        let start = std::time::Instant::now();
        run_detached(vec![("sleep", vec!["2".to_string()], None), ("sleep", vec!["2".to_string()], None)], Vec::new());
        assert!(start.elapsed() < Duration::from_millis(50), "a sequence must not wait on its children either");

        run_detached(Vec::new(), Vec::new());
    }

    /// The deadline, on the shape that made it necessary: a program
    /// that never exits (`bluetoothctl` with no `org.bluez` on the bus,
    /// and `sleep` here standing in for it) is killed and reported as
    /// *no reading*, so its widget draws a dead face rather than a
    /// number from before the tool wedged. Without this the worker
    /// thread — and the child — would be parked for the life of the
    /// session.
    #[test]
    fn a_command_that_never_exits_is_killed_and_reads_as_nothing() {
        #[allow(clippy::disallowed_methods)]
        let child = Command::new("sleep").arg("30").stdout(std::process::Stdio::piped()).spawn().expect("sleep exists");
        let start = std::time::Instant::now();
        let reading = wait_with_deadline(child, "sleep", Duration::from_millis(150));
        assert!(reading.is_none(), "a killed command has no reading, exactly as a failed one has none");
        assert!(start.elapsed() < Duration::from_secs(5), "and the wait ended at the deadline, not at the child's own pace");
    }

    /// The ordinary path is untouched by the deadline machinery: a
    /// command that exits in time still hands back its stdout, which is
    /// the whole product of a `Source::Command`.
    #[test]
    fn a_command_that_exits_in_time_still_yields_its_output() {
        #[allow(clippy::disallowed_methods)]
        let child = Command::new("echo").arg("hello").stdout(std::process::Stdio::piped()).spawn().expect("echo exists");
        assert_eq!(wait_with_deadline(child, "echo", Duration::from_secs(5)).as_deref(), Some("hello\n"));

        #[allow(clippy::disallowed_methods)]
        let child = Command::new("false").stdout(std::process::Stdio::piped()).spawn().expect("false exists");
        assert_eq!(wait_with_deadline(child, "false", Duration::from_secs(5)), None, "a non-zero exit is still no reading");
    }
}
