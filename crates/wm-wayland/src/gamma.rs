//! `zwlr_gamma_control_unstable_v1`: the protocol every night-light
//! tool speaks.
//!
//! Without this module chonkstep cannot tint or dim the screen at all,
//! by any program. `wlsunset`, `gammastep` and `redshift` all warm a
//! display through this one protocol; Hyprland's `hyprsunset` uses its
//! own `hyprland-ctm-control-v1` instead, so implementing that would
//! buy exactly one tool. Implementing this one makes the whole
//! ecosystem work at once, which is why it is the one that is here.
//!
//! The gap was found from the outside: an upstream patch to Omarchy's
//! nightlight script could not be tested on this desktop, because
//! nothing on this desktop could tint a screen.
//!
//! # What the protocol asks for
//!
//! A client binds the manager and asks for a `zwlr_gamma_control_v1`
//! for one `wl_output`. **At most one client at a time may control an
//! output's gamma** — that exclusivity is the whole point of the
//! protocol, and it is what stops two night-light daemons fighting
//! over the same screen. A second claimant is sent `failed` and its
//! object goes inert.
//!
//! The compositor answers a successful claim with `gamma_size` — the
//! ramp length the hardware wants — immediately. The client then sends
//! `set_gamma` with a file descriptor carrying three consecutive ramps
//! (red, green, blue), each `gamma_size` entries of native-endian
//! `u16`. On the client going away the original ramp is put back.
//!
//! Smithay 0.7 ships no helper for this protocol (it implements
//! `wlr-layer-shell` and `wlr-data-control`, and nothing else of the
//! wlr family), so the global, the dispatch and the state live here,
//! written directly against `wayland-protocols-wlr` on [`Compositor`]
//! itself — the same shape, and for the same reasons, that
//! `protocols.rs` and `output_mgmt.rs` document: the protocol crate is
//! already a transitive dependency, and one state type needs no
//! delegation layer.
//!
//! # Which DRM mechanism, and why
//!
//! The legacy `DRM_IOCTL_MODE_SETGAMMA` ioctl, reached through the
//! `drm` crate's `ControlDevice::set_gamma` — not the atomic
//! `GAMMA_LUT` property blob.
//!
//! Both work on this stack, so the choice is about which one this
//! codebase can perform *cleanly*:
//!
//! - The `drm` crate exposes `get_gamma`/`set_gamma` as plain methods
//!   on the control device, and smithay's `DrmDeviceFd` implements
//!   that trait. That is the whole implementation: two calls, no new
//!   dependency, no state kept anywhere but here.
//! - `GAMMA_LUT` would mean creating a property blob and committing it
//!   atomically. Every atomic commit on these crtcs is owned by
//!   smithay's `DrmCompositor` (see `session.rs`), which builds its own
//!   full property set per frame and exposes no seam for an extra
//!   property. Committing behind its back is how a compositor
//!   desynchronises its own modeset state.
//!
//! The legacy ioctl is not a legacy *path* on an atomic driver: the
//! kernel routes it through `drm_atomic_helper_legacy_gamma_set`, which
//! writes the same atomic `GAMMA_LUT` state the property would have.
//! And because smithay's commits never mention `GAMMA_LUT`, the value
//! survives every frame it queues afterwards — an unlisted property
//! keeps its value across an atomic commit.
//!
//! The one honest cost: that kernel helper performs a *blocking*
//! commit, so a `set_gamma` can wait up to about one vblank for a page
//! flip already in flight on that crtc. [`refresh`] is where the ioctl
//! is called from, once per dispatch pass, keeping only the newest ramp
//! — so a client hammering `set_gamma` in a loop costs one such wait
//! per event-loop pass rather than one per request. A night-light
//! daemon sets gamma a few times an hour.
//!
//! # The nested backend: the global is not advertised
//!
//! There is no crtc inside a window. The nested winit backend
//! therefore creates **no global at all**, and a night-light tool run
//! against a nested session says so and exits — `wlsunset` prints
//! `compositor doesn't support wlr-gamma-control-unstable-v1`.
//!
//! The alternative was to accept the ramp and apply it in the renderer
//! as a shader LUT. That is not cheap here: `render_frame_winit`
//! draws the scene straight into the host's EGL surface with a damage
//! tracker pinned to scale 1 (see `renderer.rs`'s header for how
//! load-bearing that pin is), and a per-channel LUT needs an offscreen
//! colour target plus a second pass sampling a LUT texture — a
//! rewrite of the one render path every end-to-end test in this repo
//! runs through, to buy a preview-only convenience.
//!
//! What was never on the table is advertising the global and silently
//! doing nothing: a night-light tool that reports success while the
//! screen stays blue is worse than one that fails, because the user
//! has no way to tell it apart from a broken monitor.
//!
//! The same honesty applies on real hardware. An output whose crtc
//! reports a `gamma_length` of zero cannot be controlled, and its
//! claims are answered `failed` — the protocol's own first-listed
//! reason, "the output doesn't support gamma tables" — rather than
//! accepted and dropped.
//!
//! # Testing what a night-light tool actually gets
//!
//! Everything above is decided by three pure functions ([`claim`],
//! [`restore_target`], [`Ramps::parse`]) that the unit tests at the
//! bottom of this file drive directly, and by dispatch impls that only
//! translate those decisions into events. What unit tests cannot reach
//! is the wire: whether a real client is *told* `gamma_size`, whether a
//! second one is *told* `failed`, whether a wrongly sized table earns a
//! protocol error rather than a read past the end of a buffer.
//!
//! `CHONKSTEP_TEST_GAMMA_SIZE` closes that gap. Set to a ramp length,
//! it gives the *nested* backend a stand-in for the crtc it does not
//! have: the global is advertised, the whole protocol runs for real,
//! and the ramps are recorded and logged instead of scanned out. It is
//! test apparatus in the same shape and for the same reason as
//! `CHONKSTEP_TEST_SOCKET` (see `test_door.rs`) — inert unless a test
//! sets it, announced in the log when it is not, and never on a
//! desktop.
//!
//! It is emphatically not the silent no-op ruled out above. Nothing a
//! user runs sets it; a session that has it set says so at startup and
//! on every ramp it records; and with it unset the nested backend
//! advertises nothing at all, which is what a user gets.
//!
//! `chonk-testkit`'s `chonk-gamma-probe` is the client that drives it,
//! and `tests/gamma.rs` is where exclusivity and the restore are
//! asserted end to end.
//!
//! # Integration contract
//!
//! One field on [`Compositor`], one `init` in `run` before the
//! listening socket exists, one [`refresh`] per dispatch pass, one
//! [`restore_all`] as the session ends, and one flag set from the
//! seat-activation handler in `session.rs` (a VT switch hands the crtcs
//! to another session, which resets their LUTs; [`refresh`] programs
//! the live ramp back when the seat comes home).

use std::os::fd::OwnedFd;
use std::os::unix::fs::FileExt;

use smithay::output::Output;
use smithay::reexports::wayland_protocols_wlr::gamma_control::v1::server::zwlr_gamma_control_manager_v1::{
    self, ZwlrGammaControlManagerV1,
};
use smithay::reexports::wayland_protocols_wlr::gamma_control::v1::server::zwlr_gamma_control_v1::{
    self, ZwlrGammaControlV1,
};
use smithay::reexports::wayland_server::backend::{ClientId, GlobalId, ObjectId};
use smithay::reexports::wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource};

use crate::state::Compositor;

/// The only version of this protocol that exists. `zwlr_gamma_control`
/// has never been revised — one manager request, one control request,
/// two events.
const GAMMA_CONTROL_VERSION: u32 = 1;

/// Everything the global keeps between dispatch passes: one slot per
/// output, index-aligned with `Compositor::outputs`.
pub(crate) struct GammaControl {
    /// `None` on a backend that cannot set gamma — the nested one — in
    /// which case no global was created and every function here is a
    /// no-op. Held (never read again) so the global outlives `run`.
    _global: Option<GlobalId>,
    outputs: Vec<OutputGamma>,
    /// Whether this session's ramps are recorded rather than scanned
    /// out — the `CHONKSTEP_TEST_GAMMA_SIZE` stand-in described in the
    /// module header. False in every session a person logs into.
    simulated: bool,
    /// Set by the seat-activation handler in `session.rs`. A VT switch
    /// hands the device to another session, which resets every crtc's
    /// LUT; the ramp a night-light daemon set is still *logically* in
    /// force, so [`refresh`] programs it back rather than making the
    /// daemon notice and re-send.
    reprogram_after_vt_switch: bool,
    /// The active hyprland-ctm-control manager, if any. Kept in the
    /// same state as wlr ownership so two protocols cannot fight over
    /// one CRTC.
    ctm_manager: Option<ObjectId>,
}

/// One output's gamma state.
struct OutputGamma {
    /// The ramp length this output's hardware wants, and the number
    /// [`gamma_size`](zwlr_gamma_control_v1::Event::GammaSize)
    /// advertises. Zero means the hardware cannot do it, and every
    /// claim on this output is answered `failed`.
    size: usize,
    /// The control that currently owns this output, if any. The
    /// protocol's exclusivity lives entirely in this one `Option`: a
    /// claim while it is `Some` is refused, and a `set_gamma` from any
    /// object that is not the one in here is ignored.
    owner: Option<Owner>,
    /// What the crtc's LUT held before the current owner touched it,
    /// captured at claim time. This is what a crashed night-light
    /// daemon's screen goes back to.
    original: Option<Ramps>,
    /// The ramp currently programmed, kept so a VT switch can be
    /// undone (see [`GammaControl::reprogram_after_vt_switch`]).
    live: Option<Ramps>,
    /// A ramp waiting for [`refresh`] to program it — either a client's
    /// `set_gamma` or a restore. Replaced rather than queued: only the
    /// newest ramp for an output has ever mattered.
    pending: Option<Ramps>,
}

/// Per-`zwlr_gamma_control_v1` data: which output it was created for.
/// Whether it is *live* is not stored here — that is
/// `OutputGamma::owner`, so there is exactly one place exclusivity can
/// be decided from and no second flag to fall out of step with it.
struct ControlData {
    index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Owner {
    Wlr(ObjectId),
    Ctm(ObjectId),
}

/// Three gamma ramps, one per channel, each of the output's
/// `gamma_size` entries.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Ramps {
    red: Vec<u16>,
    green: Vec<u16>,
    blue: Vec<u16>,
}

/// Why a claim on an output was refused. Both variants are answered
/// with the same `failed` event — the protocol has only the one — but
/// they are different facts about the world and the log line says
/// which.
#[derive(PartialEq, Eq, Debug)]
enum ClaimRefusal {
    /// The crtc reports a `gamma_length` of zero: this screen can never
    /// be tinted, by anyone. The protocol's first listed reason for
    /// `failed`, "the output doesn't support gamma tables".
    NoHardwareRamp,
    /// Another client already holds this output. The exclusivity the
    /// whole protocol exists for, and what stops two night-light
    /// daemons fighting over one screen.
    AlreadyOwned,
}

/// The claim decision, whole: may a new `zwlr_gamma_control_v1` take
/// this output, and at what ramp size?
///
/// Pure, and split out of the request handler on purpose — this is one
/// of the two behaviors that protect a user's screen, and a decision
/// buried inside a `Dispatch` impl cannot be tested without a display
/// and two clients.
fn claim(size: usize, already_owned: bool) -> Result<usize, ClaimRefusal> {
    if size == 0 {
        return Err(ClaimRefusal::NoHardwareRamp);
    }
    if already_owned {
        return Err(ClaimRefusal::AlreadyOwned);
    }
    Ok(size)
}

/// What an output's ramp must become once the client holding it goes
/// away: the ramp captured before that client touched anything.
///
/// The other behavior that protects a user's screen, and the reason it
/// is a named function rather than three lines inside `destroyed` — a
/// night-light daemon that crashes must not leave the screen orange,
/// and that promise deserves to be somewhere a test can reach.
///
/// `None` means leave the hardware alone: a client that claimed an
/// output and died without ever sending a ramp changed nothing, so
/// there is nothing to undo and no reason to spend a blocking commit
/// undoing it.
fn restore_target(live: Option<&Ramps>, original: Option<&Ramps>) -> Option<Ramps> {
    live.and(original).cloned()
}

/// What an output's ramp must become when the seat comes back from a
/// VT switch, which hands the crtcs to another session and resets
/// their LUTs on the way through.
///
/// Different question from [`restore_target`], and the difference is
/// the whole point: a client still holding the output wants *its* ramp
/// back, not the one from before it claimed. Only with no ramp live
/// does the original apply.
fn reprogram_target(live: Option<&Ramps>, original: Option<&Ramps>) -> Option<Ramps> {
    live.or(original).cloned()
}

/// Why a client's gamma table was rejected.
#[derive(PartialEq, Eq, Debug)]
enum RampError {
    /// The compositor advertised a ramp size of zero — this output can
    /// never be controlled, so there is no table that would be valid.
    Unsupported,
    /// The table was not exactly three ramps of `gamma_size` `u16`s.
    /// Short, long, or misaligned all land here; the protocol has one
    /// error for all of them (`invalid_gamma`).
    Length { expected: usize, got: usize },
}

impl Ramps {
    /// The identity ramp — output equals input across the range. What
    /// an untouched LUT holds, and the fallback this module restores to
    /// when the driver would not tell us what was there before.
    fn linear(size: usize) -> Self {
        let channel: Vec<u16> = (0..size)
            .map(|i| {
                if size <= 1 {
                    u16::MAX
                } else {
                    // The multiply is done in u64 so a large ramp
                    // cannot overflow the intermediate: 4096 * 65535
                    // fits in u32 today, but the arithmetic should not
                    // depend on hardware staying small.
                    ((i as u64 * u16::MAX as u64) / (size as u64 - 1)) as u16
                }
            })
            .collect();
        Self { red: channel.clone(), green: channel.clone(), blue: channel }
    }

    /// Parses the bytes of a client's gamma table: three consecutive
    /// ramps of `size` little-endian `u16`s.
    ///
    /// "Little-endian" is what the protocol means by native: every
    /// machine this compositor runs on is little-endian, the wire
    /// format is the client's own memory, and spelling it explicitly is
    /// what makes the parse a pure function that a test can feed
    /// hostile bytes to.
    ///
    /// This is the whole trust boundary for `set_gamma`. Everything the
    /// client controls — how many bytes, what they say — is checked
    /// here, and the size the ramps are read at is the *compositor's*
    /// (from the hardware), never a number the client supplied.
    fn parse(bytes: &[u8], size: usize) -> Result<Self, RampError> {
        if size == 0 {
            return Err(RampError::Unsupported);
        }
        let expected = size.checked_mul(3).and_then(|entries| entries.checked_mul(2)).ok_or(RampError::Unsupported)?;
        if bytes.len() != expected {
            return Err(RampError::Length { expected, got: bytes.len() });
        }
        let channel = |offset: usize| -> Vec<u16> {
            bytes[offset..offset + size * 2].as_chunks::<2>().0.iter().copied().map(u16::from_le_bytes).collect()
        };
        Ok(Self { red: channel(0), green: channel(size * 2), blue: channel(size * 4) })
    }
}

/// Registers the global, if this backend can honor it. Called once from
/// `run`, before the listening socket exists, like every other global —
/// a global that is missing when a client binds might as well not
/// exist.
///
/// The per-output ramp sizes come from the hardware
/// ([`crate::session::gamma_ramp_sizes`]), so an output whose crtc
/// reports zero is known to be uncontrollable before any client asks.
/// If *no* output can be controlled — every nested session, and a
/// hardware session whose driver exposes no gamma at all — no global is
/// created and a night-light tool finds nothing to bind, which is the
/// honest answer.
pub(crate) fn init(display_handle: &DisplayHandle, graphics: &crate::state::Graphics) -> GammaControl {
    let mut sizes = crate::session::gamma_ramp_sizes(graphics);
    // The end-to-end stand-in, described in the module header: a ramp
    // length for the nested backend's crtc-less output, so a real
    // client can be run against the real protocol. Only ever fills in
    // for an output the hardware said it cannot do — it never overrides
    // a real crtc's answer — and only on the nested backend, so no
    // hardware session can reach this branch at all.
    let simulated = simulated_ramp_size().filter(|_| matches!(graphics, crate::state::Graphics::Winit(_)));
    if let Some(size) = simulated {
        tracing::warn!(
            size,
            "CHONKSTEP_TEST_GAMMA_SIZE is set: this session ADVERTISES gamma control and RECORDS \
             the ramps instead of displaying them. Test apparatus — nothing on a desktop sets this."
        );
        for entry in sizes.iter_mut().filter(|size| **size == 0) {
            *entry = size;
        }
    }
    let simulated = simulated.is_some();
    let outputs: Vec<OutputGamma> = sizes
        .iter()
        .map(|&size| OutputGamma { size: size as usize, owner: None, original: None, live: None, pending: None })
        .collect();
    if outputs.iter().all(|output| output.size == 0) {
        tracing::info!(
            outputs = outputs.len(),
            "no output can set a gamma ramp on this backend; wlr-gamma-control is NOT advertised \
             (night-light tools will say so and exit rather than silently do nothing)"
        );
        return GammaControl { _global: None, outputs, simulated, reprogram_after_vt_switch: false, ctm_manager: None };
    }
    let global = display_handle.create_global::<Compositor, ZwlrGammaControlManagerV1, ()>(GAMMA_CONTROL_VERSION, ());
    tracing::info!(
        version = GAMMA_CONTROL_VERSION,
        sizes = ?sizes,
        "wlr-gamma-control advertised; wlsunset/gammastep/redshift can warm this session"
    );
    GammaControl { _global: Some(global), outputs, simulated, reprogram_after_vt_switch: false, ctm_manager: None }
}

pub(crate) fn available(gamma: &GammaControl) -> bool {
    gamma.outputs.iter().any(|output| output.size > 0)
}

/// Reserves color ownership for one CTM manager. A live wlr-gamma
/// control is a conflict too: the wire protocols differ, the CRTC does not.
pub(crate) fn claim_ctm_manager(gamma: &mut GammaControl, id: ObjectId) -> bool {
    if gamma.ctm_manager.is_some() || gamma.outputs.iter().any(|output| output.owner.is_some()) {
        return false;
    }
    gamma.ctm_manager = Some(id);
    true
}

pub(crate) fn ctm_manager_is(gamma: &GammaControl, id: &ObjectId) -> bool {
    gamma.ctm_manager.as_ref() == Some(id)
}

/// Stages a diagonal CTM as three scaled identity ramps. Outputs not
/// named in `scales` are restored to identity on this commit, matching
/// the CTM protocol's transaction semantics.
pub(crate) fn commit_ctm(comp: &mut Compositor, id: &ObjectId, scales: &[(usize, [f64; 3])]) {
    if comp.gamma.ctm_manager.as_ref() != Some(id) {
        return;
    }
    for index in 0..comp.gamma.outputs.len() {
        let diagonal =
            scales.iter().find_map(|(candidate, value)| (*candidate == index).then_some(*value)).unwrap_or([1.0; 3]);
        let slot = &mut comp.gamma.outputs[index];
        if slot.size == 0 {
            tracing::info!(index, "CTM skipped: this output has no hardware gamma ramp");
            continue;
        }
        let size = slot.size;
        if slot.original.is_none() {
            slot.original = Some(
                crate::session::read_gamma(&comp.graphics, index, size)
                    .map(|(red, green, blue)| Ramps { red, green, blue })
                    .unwrap_or_else(|| Ramps::linear(size)),
            );
        }
        let base = slot.original.as_ref().cloned().unwrap_or_else(|| Ramps::linear(size));
        let scale_channel = |channel: &[u16], factor: f64| {
            channel.iter().map(|value| ((*value as f64 * factor).round().clamp(0.0, u16::MAX as f64)) as u16).collect()
        };
        slot.owner = Some(Owner::Ctm(id.clone()));
        slot.pending = Some(Ramps {
            red: scale_channel(&base.red, diagonal[0]),
            green: scale_channel(&base.green, diagonal[1]),
            blue: scale_channel(&base.blue, diagonal[2]),
        });
    }
}

/// Releases a CTM manager and schedules restoration even if the client died.
pub(crate) fn release_ctm_manager(gamma: &mut GammaControl, id: &ObjectId) {
    if gamma.ctm_manager.as_ref() != Some(id) {
        return;
    }
    gamma.ctm_manager = None;
    for output in &mut gamma.outputs {
        if output.owner.as_ref() != Some(&Owner::Ctm(id.clone())) {
            continue;
        }
        output.owner = None;
        output.pending = restore_target(output.live.as_ref(), output.original.as_ref());
    }
}

/// The ramp length `CHONKSTEP_TEST_GAMMA_SIZE` asks for, if any. Read
/// once: a session pays one environment lookup at startup and nothing
/// else, exactly as `test_door.rs` does for its own socket variable.
/// A value that is not a positive number is ignored rather than
/// guessed at.
fn simulated_ramp_size() -> Option<u32> {
    static SIZE: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    *SIZE.get_or_init(|| {
        std::env::var("CHONKSTEP_TEST_GAMMA_SIZE")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|size| *size > 0)
    })
}

/// The once-per-pass reconciliation: program whatever ramp each output
/// is owed. Nothing at all on the passes where nothing changed, which
/// is all of them.
///
/// Every DRM ioctl this module performs happens here rather than inside
/// a request handler, for the reason the module header gives: the
/// kernel's legacy gamma path is a blocking atomic commit, and one per
/// dispatch pass with only the newest ramp kept is a bound a hostile
/// client cannot push past.
pub(crate) fn refresh(comp: &mut Compositor) {
    if comp.gamma.reprogram_after_vt_switch {
        comp.gamma.reprogram_after_vt_switch = false;
        for output in comp.gamma.outputs.iter_mut() {
            // The seat came back to a crtc whose LUT the other session
            // reset. Whatever ramp is logically in force — a client's,
            // or the original if nobody owns this output — is put back.
            if output.pending.is_none() {
                output.pending = reprogram_target(output.live.as_ref(), output.original.as_ref());
            }
        }
    }
    for index in 0..comp.gamma.outputs.len() {
        let Some(ramps) = comp.gamma.outputs[index].pending.take() else {
            continue;
        };
        match program(comp, index, &ramps) {
            Ok(()) => {
                comp.gamma.outputs[index].live = Some(ramps);
            }
            Err(error) => {
                tracing::warn!(index, %error, "could not program the gamma ramp");
                comp.gamma.outputs[index].live = None;
                // The protocol's second listed reason for `failed`:
                // "setting the gamma tables failed". The owner is told
                // and released, so the next client gets a real chance
                // rather than inheriting a wedged claim.
                if let Some(Owner::Wlr(owner)) = comp.gamma.outputs[index].owner.take() {
                    if let Ok(owner) = ZwlrGammaControlV1::from_id(&comp.display_handle, owner) {
                        owner.failed();
                    }
                }
            }
        }
    }
}

/// Programs one output with one ramp, and says so in the log.
///
/// The log line is the observable side of this module: it names the
/// white point — the last entry of each channel, which is what
/// separates a warm screen from a neutral one — so a support question
/// ("why is my screen orange?") and an end-to-end test ask the same
/// question of the same line. Ramps are set a handful of times an hour,
/// so this costs nothing to leave on.
///
/// Under the `CHONKSTEP_TEST_GAMMA_SIZE` stand-in the ioctl is skipped
/// and the line says so; there is no crtc behind a nested output to
/// program.
fn program(comp: &mut Compositor, index: usize, ramps: &Ramps) -> Result<(), String> {
    let white = |channel: &[u16]| channel.last().copied().unwrap_or_default();
    let result = if comp.gamma.simulated {
        Ok(())
    } else {
        crate::session::write_gamma(&comp.graphics, index, &ramps.red, &ramps.green, &ramps.blue)
    };
    if result.is_ok() {
        tracing::info!(
            index,
            size = ramps.red.len(),
            white_r = white(&ramps.red),
            white_g = white(&ramps.green),
            white_b = white(&ramps.blue),
            recorded_only = comp.gamma.simulated,
            "gamma ramp programmed"
        );
    }
    result
}

/// Puts every output's original ramp back, synchronously. Called as the
/// session ends — an exit, a theme pick, a hot restart — because a
/// compositor that leaves the screen orange on its way out is the exact
/// failure this module exists to prevent, and the incoming process (or
/// the login greeter) has no way to know a ramp was ever set.
pub(crate) fn restore_all(comp: &mut Compositor) {
    for index in 0..comp.gamma.outputs.len() {
        let Some(original) = comp.gamma.outputs[index].original.take() else {
            continue;
        };
        if let Err(error) = program(comp, index, &original) {
            tracing::warn!(index, %error, "could not restore the gamma ramp on the way out");
        } else {
            tracing::info!(index, "gamma ramp restored as the session ends");
        }
        comp.gamma.outputs[index].live = None;
        comp.gamma.outputs[index].pending = None;
    }
}

/// Marks every output as needing its ramp programmed again, because the
/// seat was handed back after a VT switch and the crtcs came home with
/// their LUTs reset. Called from `session.rs`'s seat-activation
/// handler; the work itself is [`refresh`]'s, one pass later, so this
/// stays a flag rather than an ioctl inside an event callback.
pub(crate) fn note_session_resumed(gamma: &mut GammaControl) {
    if gamma.outputs.iter().any(|output| output.live.is_some() || output.original.is_some()) {
        gamma.reprogram_after_vt_switch = true;
    }
}

/// Rebuilds the index-aligned hardware slots after connector hotplug.
/// Existing controls become inert (their recorded object id owns no
/// new slot), preventing a stale handle from programming a different
/// CRTC that happened to inherit its old index.
pub(crate) fn outputs_changed(
    gamma: &mut GammaControl,
    graphics: &crate::state::Graphics,
    display_handle: &DisplayHandle,
) {
    let sizes = crate::session::gamma_ramp_sizes(graphics);
    gamma.outputs = sizes
        .into_iter()
        .map(|size| OutputGamma {
            size: size as usize,
            owner: None,
            original: None,
            live: None,
            pending: None,
        })
        .collect();
    gamma.ctm_manager = None;
    gamma.reprogram_after_vt_switch = false;
    if gamma._global.is_none() && gamma.outputs.iter().any(|output| output.size > 0) {
        gamma._global = Some(
            display_handle.create_global::<Compositor, ZwlrGammaControlManagerV1, ()>(
                GAMMA_CONTROL_VERSION,
                (),
            ),
        );
    }
}

/// Which entry of `Compositor::outputs` a `wl_output` names, or `None`
/// for a resource that is not one of ours. Same derivation as
/// `lock.rs` and `protocols.rs` use — the client named a specific
/// output and `Output::from_resource` answers exactly that.
pub(crate) fn output_index(
    comp: &Compositor,
    resource: &smithay::reexports::wayland_server::protocol::wl_output::WlOutput,
) -> Option<usize> {
    let output = Output::from_resource(resource)?;
    comp.outputs.iter().position(|entry| entry.output == output)
}

/// Reads a client's gamma table off the file descriptor it sent.
///
/// `pread` rather than `read`, and that is the load-bearing choice: a
/// client is free to hand over any fd at all, including a pipe or a
/// socket with nothing in it, and a plain blocking `read` on one of
/// those would freeze the compositor's single event loop for as long as
/// the client felt like it. `pread` is defined only for seekable
/// objects, so a pipe fails immediately with `ESPIPE` instead of
/// hanging — the hostile case turns into an error return before it can
/// turn into a frozen desktop. It also ignores the fd's own offset,
/// which the protocol says nothing about.
///
/// The buffer is sized from the *hardware's* ramp size, so a client
/// cannot make this allocate: a file claiming to be gigabytes long is
/// read for exactly `size * 3 * 2` bytes and then checked for being
/// longer than that.
fn read_table(fd: OwnedFd, size: usize) -> Result<Ramps, String> {
    let expected = size.saturating_mul(6);
    let file = std::fs::File::from(fd);
    let mut buffer = vec![0u8; expected];
    let mut filled = 0usize;
    while filled < expected {
        match file.read_at(&mut buffer[filled..], filled as u64) {
            // A short read that is not EOF is legal for `pread`; only
            // zero means the file really is shorter than it must be.
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(error) => return Err(format!("could not read the gamma table: {error}")),
        }
    }
    if filled != expected {
        return Err(format!("gamma table is {filled} bytes, expected {expected}"));
    }
    // The protocol says the file must have the same length as three
    // ramps, not "at least". One byte past the end is a client that
    // disagrees with us about the ramp size, and honoring the prefix
    // would tint the screen with a table meant for other hardware.
    let mut extra = [0u8; 1];
    match file.read_at(&mut extra, expected as u64) {
        Ok(0) => {}
        Ok(_) => return Err(format!("gamma table is longer than the expected {expected} bytes")),
        // An error reading *past* the table is not a reason to refuse a
        // table that has already been read in full.
        Err(_) => {}
    }
    Ramps::parse(&buffer, size).map_err(|error| match error {
        RampError::Unsupported => "this output cannot set a gamma ramp".to_string(),
        RampError::Length { expected, got } => {
            format!("gamma table is {got} bytes, expected {expected}")
        }
    })
}

// ---------------------------------------------------------------------
// Dispatch.
// ---------------------------------------------------------------------

impl GlobalDispatch<ZwlrGammaControlManagerV1, ()> for Compositor {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrGammaControlManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        // The manager carries no state of its own: every request on it
        // mints a control, and the controls are what this module
        // tracks.
        data_init.init(resource, ());
    }

    fn can_view(client: Client, _global_data: &()) -> bool {
        crate::state::privileged_global_visible(&client)
    }
}

impl Dispatch<ZwlrGammaControlManagerV1, ()> for Compositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &ZwlrGammaControlManagerV1,
        request: zwlr_gamma_control_manager_v1::Request,
        _data: &(),
        _display_handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwlr_gamma_control_manager_v1::Request::GetGammaControl { id, output } => {
                // The index is resolved before the resource exists so a
                // `wl_output` from some other compositor's world — or
                // one already being torn down — produces an inert
                // control rather than a panic. `usize::MAX` can never
                // match a real slot, so such a control owns nothing and
                // every later request on it is ignored.
                let index = output_index(state, &output);
                let control = data_init.init(id, ControlData { index: index.unwrap_or(usize::MAX) });
                let Some(index) = index else {
                    tracing::debug!("gamma control asked for an output that is not ours");
                    control.failed();
                    return;
                };
                let Some(slot) = state.gamma.outputs.get_mut(index) else {
                    control.failed();
                    return;
                };
                let size = match claim(slot.size, slot.owner.is_some() || state.gamma.ctm_manager.is_some()) {
                    Ok(size) => size,
                    Err(refusal) => {
                        let reason = match refusal {
                            ClaimRefusal::NoHardwareRamp => "this output's hardware has no gamma ramp",
                            ClaimRefusal::AlreadyOwned => "another client already owns this output",
                        };
                        tracing::info!(index, reason, "gamma control refused");
                        control.failed();
                        return;
                    }
                };
                // Capture what the screen looks like now, so a client
                // that crashes can be undone. A driver that refuses to
                // read its own LUT back leaves us the identity ramp,
                // which is what an untouched crtc holds anyway — the
                // one thing that must never happen is having nothing to
                // restore to.
                if slot.original.is_none() {
                    slot.original = Some(
                        crate::session::read_gamma(&state.graphics, index, size)
                            .map(|(red, green, blue)| Ramps { red, green, blue })
                            .unwrap_or_else(|| {
                                tracing::info!(
                                    index,
                                    "the driver would not read its gamma ramp back; \
                                     a linear ramp is what this output will be restored to"
                                );
                                Ramps::linear(size)
                            }),
                    );
                }
                slot.owner = Some(Owner::Wlr(control.id()));
                // "This event is sent immediately when the gamma control
                // object is created."
                control.gamma_size(size as u32);
                tracing::info!(index, size, "gamma control claimed");
            }
            zwlr_gamma_control_manager_v1::Request::Destroy => {
                // "All objects created by the manager will still remain
                // valid" — nothing to do; the controls outlive it.
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrGammaControlV1, ControlData> for Compositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZwlrGammaControlV1,
        request: zwlr_gamma_control_v1::Request,
        data: &ControlData,
        _display_handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwlr_gamma_control_v1::Request::SetGamma { fd } => {
                let index = data.index;
                let Some(slot) = state.gamma.outputs.get_mut(index) else {
                    // An inert control (a refused claim). The fd is
                    // dropped — and so closed — with this scope.
                    return;
                };
                // The object was sent `failed` and is inert, or it never
                // owned this output. Either way it does not get to move
                // the screen; the owner is the only object that can.
                if slot.owner.as_ref() != Some(&Owner::Wlr(resource.id())) {
                    return;
                }
                let size = slot.size;
                match read_table(fd, size) {
                    Ok(ramps) => {
                        // Parked for `refresh`, which is the only place
                        // that touches the hardware. Replacing any ramp
                        // already parked is deliberate: a client that
                        // sets gamma twice in one pass meant the second
                        // one.
                        slot.pending = Some(ramps);
                    }
                    Err(reason) => {
                        tracing::info!(index, %reason, "gamma table refused");
                        resource.post_error(zwlr_gamma_control_v1::Error::InvalidGamma, reason);
                    }
                }
            }
            zwlr_gamma_control_v1::Request::Destroy => {
                // "If the object is still valid, this restores the
                // original gamma tables." Handled in `destroyed`, which
                // runs for this request *and* for a client that died
                // without sending it — and the client that died without
                // sending it is the case that matters.
            }
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, resource: &ZwlrGammaControlV1, data: &ControlData) {
        let Some(slot) = state.gamma.outputs.get_mut(data.index) else {
            return;
        };
        if slot.owner.as_ref() != Some(&Owner::Wlr(resource.id())) {
            return;
        }
        slot.owner = None;
        // The restore, and the reason this module has a `destroyed`
        // hook at all: a night-light daemon that crashes must not leave
        // the screen orange. Parked for `refresh` rather than performed
        // here for the same blocking-ioctl reason every other apply is
        // — and a client disconnecting is itself an event, so the pass
        // that runs `refresh` is the very next one.
        slot.pending = restore_target(slot.live.as_ref(), slot.original.as_ref());
        if slot.pending.is_some() {
            tracing::info!(index = data.index, "gamma control released; restoring the original ramp");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A well-formed table for `size` entries, with a recognisable
    /// value per channel so a mixed-up channel order is visible.
    fn table(size: usize) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(size * 6);
        for channel in 0..3u16 {
            for entry in 0..size as u16 {
                bytes.extend_from_slice(&(entry + channel * 1000).to_le_bytes());
            }
        }
        bytes
    }

    #[test]
    fn a_well_formed_table_parses_into_three_channels_in_order() {
        let ramps = Ramps::parse(&table(4), 4).expect("a correctly sized table is valid");
        assert_eq!(ramps.red, vec![0, 1, 2, 3]);
        assert_eq!(ramps.green, vec![1000, 1001, 1002, 1003]);
        assert_eq!(ramps.blue, vec![2000, 2001, 2002, 2003]);
    }

    #[test]
    fn hostile_tables_are_refused_rather_than_read_past_the_end() {
        // Short: one byte missing. The classic crash — a compositor
        // that trusts the length and indexes anyway reads off the end.
        let mut short = table(256);
        short.pop();
        assert_eq!(Ramps::parse(&short, 256), Err(RampError::Length { expected: 1536, got: 1535 }));
        // Empty, which is what an fd to an empty memfd produces.
        assert_eq!(Ramps::parse(&[], 256), Err(RampError::Length { expected: 1536, got: 0 }));
        // Oversized: a table for bigger hardware than this output has.
        assert_eq!(Ramps::parse(&table(1024), 256), Err(RampError::Length { expected: 1536, got: 6144 }));
        // Misaligned: an odd byte count can never be whole u16s.
        assert_eq!(Ramps::parse(&[0u8; 7], 256), Err(RampError::Length { expected: 1536, got: 7 }));
        // A table for an output that cannot do gamma at all is refused
        // whatever it contains, including when it is empty and
        // therefore "the right length" for a zero-entry ramp.
        assert_eq!(Ramps::parse(&[], 0), Err(RampError::Unsupported));
    }

    #[test]
    fn the_parse_never_borrows_a_length_from_the_client() {
        // The same bytes read at two different sizes: only the size the
        // *compositor* passes decides, and the mismatch is caught. This
        // is the property that makes a lying client harmless.
        let bytes = table(8);
        assert!(Ramps::parse(&bytes, 8).is_ok());
        assert!(Ramps::parse(&bytes, 7).is_err());
        assert!(Ramps::parse(&bytes, 9).is_err());
    }

    #[test]
    fn a_linear_ramp_spans_the_whole_range() {
        let ramps = Ramps::linear(256);
        assert_eq!(ramps.red.len(), 256);
        assert_eq!(ramps.red[0], 0);
        assert_eq!(ramps.red[255], u16::MAX);
        assert_eq!(ramps.red, ramps.green);
        assert_eq!(ramps.green, ramps.blue);
        // Monotonic, which is what makes it the identity rather than
        // merely a ramp with the right endpoints.
        assert!(ramps.red.windows(2).all(|pair| pair[0] < pair[1]));
        // Degenerate sizes must not divide by zero.
        assert_eq!(Ramps::linear(1).red, vec![u16::MAX]);
        assert!(Ramps::linear(0).red.is_empty());
    }

    // --- Exclusivity and the restore.
    //
    // The two behaviors that protect a user's screen are decisions, and
    // they are made by `claim` and `restore_target` — pure functions
    // the dispatch impls above call rather than logic buried inside
    // them. That is what lets both be tested without a display, two
    // clients and a monitor. A probe client (`chonk-gamma-probe`)
    // drives the same decisions over the wire; these pin them.

    #[test]
    fn only_one_client_at_a_time_may_own_an_output() {
        // The first claimant on capable hardware gets the ramp size.
        assert_eq!(claim(256, false), Ok(256));
        // The second is refused while the first still holds it. Without
        // this branch two night-light daemons fight over one screen,
        // each undoing the other every few seconds.
        assert_eq!(claim(256, true), Err(ClaimRefusal::AlreadyOwned));
        // And the moment the first releases, the next one may have it —
        // exclusivity, not first-come-forever.
        assert_eq!(claim(256, false), Ok(256));
    }

    #[test]
    fn hardware_with_no_gamma_ramp_refuses_everyone() {
        // A crtc whose gamma_length is zero cannot be tinted by anyone,
        // and says so rather than accepting a table it will drop. This
        // refusal outranks the ownership one: nobody can own what does
        // not exist.
        assert_eq!(claim(0, false), Err(ClaimRefusal::NoHardwareRamp));
        assert_eq!(claim(0, true), Err(ClaimRefusal::NoHardwareRamp));
    }

    #[test]
    fn losing_the_owner_restores_the_ramp_from_before_it_claimed() {
        // The state a crashed night-light daemon leaves behind: an
        // original captured at claim time, and its warm ramp live.
        let original = Ramps::linear(256);
        let warm = Ramps { red: vec![7; 256], green: vec![5; 256], blue: vec![3; 256] };
        assert_eq!(
            restore_target(Some(&warm), Some(&original)),
            Some(original.clone()),
            "the screen must go back to what it was, not stay orange"
        );
    }

    #[test]
    fn a_client_that_never_set_a_ramp_leaves_the_screen_alone() {
        // Claiming an output and dying without ever sending `set_gamma`
        // changed nothing, so there is nothing to undo — and a needless
        // restore is a needless blocking commit.
        let original = Ramps::linear(256);
        assert_eq!(restore_target(None, Some(&original)), None);
        // Nor is there anything to restore *to* if the claim never got
        // as far as capturing one.
        assert_eq!(restore_target(Some(&original), None), None);
    }

    #[test]
    fn a_vt_switch_reprograms_whatever_is_logically_in_force() {
        // The seat comes home to crtcs the other session reset. A
        // client still holding the output wants ITS ramp back — the
        // difference from `restore_target`, and getting it wrong makes
        // every Ctrl+Alt+F2 turn the night light off.
        let original = Ramps::linear(256);
        let warm = Ramps { red: vec![7; 256], green: vec![5; 256], blue: vec![3; 256] };
        assert_eq!(reprogram_target(Some(&warm), Some(&original)), Some(warm));
        // With no client, the captured original goes back.
        assert_eq!(reprogram_target(None, Some(&original)), Some(original));
        // An output nobody has ever claimed is left alone.
        assert_eq!(reprogram_target(None, None), None);
    }
}
