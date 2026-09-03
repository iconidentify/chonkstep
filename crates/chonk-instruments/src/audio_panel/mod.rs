//! The audio device panel: the sound tile's fold-out, listing every
//! PipeWire sink with its default lamp, level, mute state and port
//! availability, with a click to make a sink the default and a click to
//! mute one — the desktop's first-party answer to "switch audio
//! devices".
//!
//! # Data model
//!
//! Everything here folds from three `pactl` surfaces, because `pactl`
//! is the only tool in the stack with machine-readable output (`wpctl`
//! has no JSON and cannot list ports):
//!
//! * `pactl --format=json list sinks` → [`parse_sinks`]: one
//!   [`AudioSink`] per device — description, mute, level, whether any
//!   port is physically present, whether it is currently rendering
//!   audio.
//! * `pactl get-default-sink` → [`parse_default_sink`]: the name of
//!   the default, matched against the list by *name*.
//! * `pactl --format=json list sink-inputs` → [`parse_sink_inputs`]:
//!   the playing streams a default-switch must carry across.
//!
//! **Sinks are keyed by `name` everywhere.** Verified on real
//! hardware: `wpctl`'s PipeWire object ids and `pactl`'s sink indexes
//! are different namespaces for the same device (the mission's Volt 4
//! is pactl index 68 and a different wpctl id), so no id ever crosses
//! from one tool to the other. The name is the one identity both ends
//! agree on, and every action below resolves it fresh.
//!
//! # The switch recipe
//!
//! `wpctl set-default` alone is not a device switch: streams that are
//! already playing stay on the old sink. The recipe — reimplemented
//! from Omarchy's `omarchy-audio-output-set-default`, not exec'd, since
//! this desktop must not require Omarchy — is:
//!
//! 1. `pactl set-default-sink <name>`;
//! 2. `pactl move-sink-input <index> <name>` for every current stream
//!    that is a real application's. A stream with no
//!    `application.name` is a DSP chain's own plumbing, and EasyEffects
//!    in particular routes its processed output through a sink input —
//!    moving either rewires the processing itself (onto headphones, or
//!    into its own virtual sink, which is a cycle). [`eligible_moves`]
//!    is that filter.
//!
//! Everything runs as the plain user session; nothing here wants or
//! gets privileges.
//!
//! # The prediction
//!
//! A click marks the clicked row default immediately and remembers
//! that as a *prediction*, not a fact — the `chonk-switch` lesson: the
//! next `pactl get-default-sink` sample is the truth, the lamp is an
//! animation of what the click asked for. A confirming sample retires
//! the prediction silently; [`PREDICTION_SAMPLES`] unconfirming
//! samples (~2.5s against the resample-then-1Hz cadence) expire it and
//! the lamp falls back to whatever the mixer actually says.
//!
//! # What this module is not
//!
//! It performs nothing. Parsing is pure functions over sampled text,
//! actions come out as [`Effect::Run`] argv for the dock to execute
//! off-thread, and the interaction state ([`AudioPanel`]) is a plain
//! state machine over panel-local points — all of it fixture-tested
//! with no audio stack behind it.

mod json;
pub mod render;

use chonk_dock_widget::{Effect, SourceId};
use json::Json;
use wm_theme::instrument_panel;
use wm_theme_api::Point;

/// One sink, as the panel models it. `PartialEq`/`Eq` so a fold can
/// answer "repaint needed" as a comparison, the way every instrument
/// state here does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioSink {
    /// The stable cross-tool key — see the module doc. Never shown.
    pub name: String,
    /// What the row shows: `pactl`'s human description, falling back
    /// to the name for a sink that somehow has none.
    pub description: String,
    pub muted: bool,
    /// First channel's `value_percent`, as `pactl` reports it (it can
    /// exceed 100 in overdrive). `None` when the sink reported no
    /// volume at all — rendered as a blank readout, not a fake zero.
    pub volume_percent: Option<u32>,
    /// Omarchy's availability rule: a sink with no ports counts as
    /// available (virtual sinks have none), otherwise at least one
    /// port must not be `"not available"`. Unavailable rows render
    /// greyed and refuse clicks.
    pub available: bool,
    /// `state == "RUNNING"`: the sink is rendering audio right now.
    pub running: bool,
}

/// One playing stream, for the switch migration. `app_name` is
/// `properties["application.name"]`; its absence is what marks a DSP
/// chain's internal stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SinkInput {
    pub index: u64,
    pub app_name: Option<String>,
}

/// The stream EasyEffects plays its processed chain through — the one
/// *named* stream the migration must still leave alone.
const EASYEFFECTS: &str = "EasyEffects";

/// Parses `pactl --format=json list sinks`. `None` for a document that
/// does not parse or is not an array — the panel's answer is the dead
/// face, same as no `pactl` at all. Entries without a name are
/// dropped individually: one malformed sink must not hide the others.
pub fn parse_sinks(text: &str) -> Option<Vec<AudioSink>> {
    let doc = Json::parse(text)?;
    let entries = doc.as_array()?;
    Some(entries.iter().filter_map(parse_sink).collect())
}

fn parse_sink(entry: &Json) -> Option<AudioSink> {
    let name = entry.get("name")?.as_str()?.to_string();
    let description = entry
        .get("description")
        .and_then(Json::as_str)
        .filter(|d| !d.trim().is_empty())
        .unwrap_or(&name)
        .to_string();
    let muted = entry.get("mute").and_then(Json::as_bool).unwrap_or(false);
    let running = entry.get("state").and_then(Json::as_str) == Some("RUNNING");
    let volume_percent = entry
        .get("volume")
        .and_then(|volume| match volume {
            Json::Obj(channels) => channels.first().map(|(_, v)| v),
            _ => None,
        })
        .and_then(|channel| channel.get("value_percent"))
        .and_then(Json::as_str)
        .and_then(parse_percent);
    let available = match entry.get("ports").and_then(Json::as_array) {
        None | Some([]) => true,
        Some(ports) => ports
            .iter()
            .any(|port| port.get("availability").and_then(Json::as_str) != Some("not available")),
    };
    Some(AudioSink { name, description, muted, volume_percent, available, running })
}

/// `"40%"` → `40`. Accepts a fractional percent by rounding — `pactl`
/// prints integers today, and a future decimal must not blank the
/// readout.
fn parse_percent(text: &str) -> Option<u32> {
    let number: f64 = text.trim().strip_suffix('%')?.trim().parse().ok()?;
    (number.is_finite() && number >= 0.0).then_some(number.round() as u32)
}

/// Parses `pactl get-default-sink`: one sink name on one line. `None`
/// for anything that does not look like one (an error message, an
/// empty read) — sink names never contain whitespace, so a line with
/// any is some other tool output.
pub fn parse_default_sink(text: &str) -> Option<String> {
    let line = text.trim();
    (!line.is_empty() && !line.contains(char::is_whitespace)).then(|| line.to_string())
}

/// Parses `pactl --format=json list sink-inputs`. Damage degrades to
/// an empty list rather than `None`: a switch with no migration list
/// still switches the default, which beats refusing the click.
pub fn parse_sink_inputs(text: &str) -> Vec<SinkInput> {
    let Some(doc) = Json::parse(text) else { return Vec::new() };
    let Some(entries) = doc.as_array() else { return Vec::new() };
    entries
        .iter()
        .filter_map(|entry| {
            let index = entry.get("index")?.as_u64()?;
            let app_name = entry
                .get("properties")
                .and_then(|p| p.get("application.name"))
                .and_then(Json::as_str)
                .map(str::to_string);
            Some(SinkInput { index, app_name })
        })
        .collect()
}

/// The streams a default-switch carries to the new sink: real
/// applications only. No `application.name` = a filter chain's own
/// plumbing; EasyEffects' named output stream is plumbing too. Both
/// stay put — see the module doc for the cycle this avoids.
pub fn eligible_moves(inputs: &[SinkInput]) -> Vec<u64> {
    inputs
        .iter()
        .filter(|input| input.app_name.as_deref().is_some_and(|app| app != EASYEFFECTS))
        .map(|input| input.index)
        .collect()
}

/// What a completed press-release on the panel asks for. The argv it
/// becomes is [`action_effects`]; keeping the decision and the argv
/// separate is what lets the gating tests say "this click means switch"
/// without asserting command lines, and the argv tests say "a switch is
/// these commands" without a pointer in sight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelAction {
    /// Make this sink the default and migrate the playing streams.
    SwitchTo(String),
    /// Toggle this one sink's mute, default or not.
    ToggleMute(String),
    /// Set this sink to an absolute level — the wheel's action, in the
    /// same 5% quantum the tile's own knob turns in.
    SetVolume { sink: String, percent: u32 },
}

/// One wheel notch, as a percentage of full scale. The same quantum
/// the sound tile's zones and wheel move in, so the two knobs feel like
/// one control at two sizes.
pub const VOLUME_STEP: u32 = 5;

/// Where the wheel stops climbing. `wpctl` enforces the tile's ceiling
/// with `-l 1.0`; `pactl` has no such flag, so the panel computes the
/// absolute target from the sampled level and stops there itself.
///
/// A sink something else pushed *above* the ceiling keeps what it has:
/// scrolling up on it asks for nothing (the wheel will not add
/// overdrive) and scrolling down steps it back down normally. The
/// panel is a knob, not a corrector — but it is not a knob that goes
/// to eleven either.
pub const MAX_VOLUME: u32 = 100;

/// The argv for one panel action, as dock effects. All `pactl`, all
/// addressed by sink *name* (never an index — see the module doc), all
/// plain user-session commands.
///
/// `confirm` is the source whose next sample proves what the action
/// did — the default-sink sampler for a switch, the sink-list sampler
/// for a mute — hung off the command that changes that reading. The
/// migration moves carry no confirm of their own: they alter which
/// streams play where, not anything the panel draws.
pub fn action_effects(action: &PanelAction, inputs: &[SinkInput], confirm: Option<SourceId>) -> Vec<Effect> {
    match action {
        PanelAction::SwitchTo(name) => {
            let mut effects = vec![Effect::Run {
                program: "pactl",
                args: vec!["set-default-sink".to_string(), name.clone()],
                then: confirm,
            }];
            effects.extend(eligible_moves(inputs).into_iter().map(|index| Effect::Run {
                program: "pactl",
                args: vec!["move-sink-input".to_string(), index.to_string(), name.clone()],
                then: None,
            }));
            effects
        }
        PanelAction::ToggleMute(name) => vec![Effect::Run {
            program: "pactl",
            args: vec!["set-sink-mute".to_string(), name.clone(), "toggle".to_string()],
            then: confirm,
        }],
        PanelAction::SetVolume { sink, percent } => vec![Effect::Run {
            program: "pactl",
            args: vec!["set-sink-volume".to_string(), sink.clone(), format!("{percent}%")],
            then: confirm,
        }],
    }
}

// ---------------------------------------------------------------------
// Panel geometry
// ---------------------------------------------------------------------

/// The floor a compressed row will not go below — the panel
/// vocabulary's own hit-target floor, because a row *is* a control.
/// Under this many device pixels the lamp, the description, the meter
/// and the mute key stop being separable marks and the row is a smear,
/// so a grant too short for every device at this height loses the tail
/// rather than the legibility.
pub const MIN_ROW_H: u32 = instrument_panel::MIN_HIT;

/// The built-in themes' tile bevel. Hit tests get no theme (the
/// soundctl zone-map precedent), and every built-in theme's tile bevel
/// is one device pixel; a theme with a wider one shifts the drawn
/// bands by a pixel or two, well inside a click target's slack.
const BEVEL: u32 = 1;

/// How far the glass sits inside the granted content rect: the gasket
/// course plus the well's lip, as [`instrument_panel::draw_panel_ground`]
/// lays it. Everything the panel draws and everything it hit-tests
/// lives inside this.
const INSET: i32 = (BEVEL * 2 + 1) as i32;

/// The panel's layout numbers, derived from the dock's tile edge so the
/// panel scales with the dock it unfolds from (56px tiles ask for a
/// 336px-wide panel, 112px HiDPI tiles for twice that) — and then from
/// the size the shell actually *granted*, which is the one that decides
/// where anything is drawn.
///
/// Both the renderer and the hit-testing read the same struct, so a
/// click lands on exactly the row it looks like it landed on. Build it
/// with [`PanelMetrics::granted`] from a [`PanelFrame`]'s size; ask for
/// a size with [`PanelMetrics::request`].
///
/// [`PanelFrame`]: chonk_dock_widget::PanelFrame
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelMetrics {
    /// Granted content width, chrome excluded (the shell draws the
    /// chrome).
    pub width: u32,
    /// Granted content height. The face fills it — a short grant is
    /// still a whole panel, never a buffer with a hole under it.
    pub height: u32,
    pub row_h: u32,
    /// Padding between the glass edge and what is on it.
    pub pad: u32,
    /// Vertical gap between rows — one engraved groove wide, since
    /// that is exactly what sits in it.
    pub gap: u32,
    /// The OUTPUTS band at the top of the glass. Constant for a tile
    /// size: a header names the rows, so it must not shrink with them
    /// when a clamped grant compresses the stack.
    pub header_h: u32,
}

impl PanelMetrics {
    /// The unclamped numbers for a tile edge: what the panel would like
    /// if the workarea never argued. Only [`request`](Self::request)
    /// and [`granted`](Self::granted) use it — nothing draws against a
    /// size that was never granted.
    fn natural(tile: u32) -> PanelMetrics {
        let tile = tile.max(8);
        let row_h = (tile * 4 / 7).max(18);
        PanelMetrics {
            width: tile * 7,
            height: 0,
            row_h,
            pad: (tile / 14).max(3),
            gap: (tile / 28).max(2),
            header_h: instrument_panel::section_h(row_h),
        }
    }

    /// The fixed furniture above the first row: the glass inset, the
    /// glass's own top padding, the OUTPUTS band, and the groove-wide
    /// gap under it.
    fn chrome_h(&self) -> u32 {
        INSET as u32 + self.pad + self.header_h + self.gap
    }

    /// The content size to ask the shell for: the header plus every
    /// device at the natural row height. The empty panel still asks for
    /// one row's worth of face, for its "no devices" reading.
    pub fn request(tile: u32, rows: usize) -> (u32, u32) {
        let m = PanelMetrics::natural(tile);
        let rows = rows.max(1) as u32;
        (m.width, m.chrome_h() + rows * m.row_h + (rows - 1) * m.gap + m.pad + INSET as u32)
    }

    /// The metrics for a grant. The width is the granted width
    /// verbatim; the row height is the natural one, compressed if — and
    /// only if — that is what it takes to fit every device inside a
    /// grant the shell clamped, down to [`MIN_ROW_H`].
    ///
    /// A grant *taller* than the request is not stretched: rows keep
    /// their natural height and the extra is glass, because a
    /// three-device panel with 60px rows would read as a menu, not as
    /// an instrument.
    pub fn granted(tile: u32, width: u32, height: u32, rows: usize) -> PanelMetrics {
        let base = PanelMetrics::natural(tile);
        let n = rows.max(1) as u32;
        let stack = height.saturating_sub(base.chrome_h() + base.pad + INSET as u32 + (n - 1) * base.gap);
        PanelMetrics { width, height, row_h: base.row_h.min(stack / n).max(MIN_ROW_H), ..base }
    }

    /// The top edge of the row stack, content-local — under the
    /// OUTPUTS band.
    pub fn rows_top(&self) -> i32 {
        self.chrome_h() as i32
    }

    /// The glass's left edge and width — the band the header and the
    /// rows are laid across.
    pub fn glass_x(&self) -> i32 {
        INSET
    }

    pub fn glass_w(&self) -> u32 {
        (self.width as i32 - INSET * 2).max(0) as u32
    }

    /// How many of `rows` devices fit whole in the granted height. A
    /// clamp too tight even for [`MIN_ROW_H`] rows shows the ones that
    /// fit and hides the rest — and hides them from the hit-test too,
    /// so nothing invisible is ever clickable.
    pub fn visible_rows(&self, rows: usize) -> usize {
        let room = self.height as i32 - INSET - self.rows_top();
        if room < self.row_h as i32 {
            return 0;
        }
        let pitch = (self.row_h + self.gap).max(1) as i32;
        rows.min(1 + ((room - self.row_h as i32) / pitch) as usize)
    }

    /// The top edge of row `i`, content-local.
    pub fn row_top(&self, i: usize) -> i32 {
        self.rows_top() + i as i32 * (self.row_h + self.gap) as i32
    }

    /// The row index at a content-local point, if it is on a *visible*
    /// row rather than in a groove, the header, the gasket, or past the
    /// grant.
    pub fn row_at(&self, point: Point, rows: usize) -> Option<usize> {
        if point.x < INSET || point.x >= self.width as i32 - INSET || point.y < self.rows_top() {
            return None;
        }
        let pitch = (self.row_h + self.gap) as i32;
        let offset = point.y - self.rows_top();
        let row = offset / pitch;
        let within = offset % pitch;
        (within < self.row_h as i32 && (row as usize) < self.visible_rows(rows)).then_some(row as usize)
    }

    /// The mute key's edge: a square control, one row tall, at the
    /// row's right end. The rest of the row is the switch-default
    /// target.
    pub fn mute_key_w(&self) -> u32 {
        instrument_panel::hit_size(self.row_h)
    }

    /// The engraved seam a row's meaning changes at: left of it the row
    /// is "make this the default", right of it is the mute key.
    ///
    /// On a grant too narrow to hold both, the key is the one that goes
    /// — this answers past the panel's right edge, where the zone is
    /// neither drawn nor clickable — rather than eating the row and
    /// turning every press on a device into a mute. Both the renderer
    /// and the hit-test read this one function, so they cannot disagree
    /// about where the seam went.
    pub fn mute_zone_left(&self) -> i32 {
        let seam = self.width as i32 - INSET - self.pad as i32 - self.mute_key_w() as i32;
        // Never closer in than two row-heights: past that the key would
        // be eating the device row rather than sitting beside it.
        if seam < INSET + self.row_h as i32 * 2 {
            return self.width as i32;
        }
        seam
    }
}

/// Which control of which row a content-local point is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelZone {
    /// The row body: make this sink the default.
    Row,
    /// The speaker square: toggle this sink's mute.
    Mute,
}

/// A control identified by the sink's *name*, not its row index, so a
/// press survives the list reordering underneath it — releasing on
/// what became a different device must not fire the old row's action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelTarget {
    pub sink: String,
    pub zone: PanelZone,
}

// ---------------------------------------------------------------------
// Panel interaction state
// ---------------------------------------------------------------------

/// How many fresh default-sink samples an unconfirmed switch
/// prediction survives. The switch command's completion triggers an
/// immediate resample and the sampler then ticks at the dock interval,
/// so three readings is roughly 2.5 seconds of believing the click —
/// `chonk-switch`'s grace, arrived at the sampled way.
pub const PREDICTION_SAMPLES: u8 = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Prediction {
    name: String,
    samples_left: u8,
}

/// The panel's whole interaction state: the folded readings plus
/// hover, the in-flight press, and the optimistic default. Pure — the
/// widget folds samples in, hands pointer events over, and takes
/// effects out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AudioPanel {
    sinks: Vec<AudioSink>,
    default_sink: Option<String>,
    inputs: Vec<SinkInput>,
    hover: Option<PanelTarget>,
    pressed: Option<PanelTarget>,
    prediction: Option<Prediction>,
}

impl AudioPanel {
    pub fn new() -> AudioPanel {
        AudioPanel::default()
    }

    pub fn sinks(&self) -> &[AudioSink] {
        &self.sinks
    }

    pub fn inputs(&self) -> &[SinkInput] {
        &self.inputs
    }

    /// The sink whose lamp is lit: the prediction while one is alive,
    /// the sampled truth otherwise.
    pub fn shown_default(&self) -> Option<&str> {
        self.prediction.as_ref().map(|p| p.name.as_str()).or(self.default_sink.as_deref())
    }

    /// The hovered target, for the renderer's highlight.
    pub fn hover(&self) -> Option<&PanelTarget> {
        self.hover.as_ref()
    }

    /// The pressed target, for the renderer's sunken row.
    pub fn pressed(&self) -> Option<&PanelTarget> {
        self.pressed.as_ref()
    }

    /// Folds a fresh sink-list reading. Returns whether the panel's
    /// pixels changed.
    pub fn fold_sinks(&mut self, reading: Option<Vec<AudioSink>>) -> bool {
        let sinks = reading.unwrap_or_default();
        let changed = sinks != self.sinks;
        self.sinks = sinks;
        changed
    }

    /// Folds a fresh default-sink reading and reconciles the
    /// prediction against it: a confirming reading retires the
    /// prediction, an unconfirming one spends a grace sample, and the
    /// lamp repaints only when the *shown* default actually moved.
    pub fn fold_default(&mut self, reading: Option<String>) -> bool {
        let shown_before = self.shown_default().map(str::to_string);
        if let Some(prediction) = &mut self.prediction {
            if reading.as_deref() == Some(prediction.name.as_str()) {
                self.prediction = None;
            } else {
                prediction.samples_left = prediction.samples_left.saturating_sub(1);
                if prediction.samples_left == 0 {
                    self.prediction = None;
                }
            }
        }
        self.default_sink = reading;
        self.shown_default().map(str::to_string).as_deref() != shown_before.as_deref()
    }

    /// Folds a fresh stream list. Streams are invisible — they only
    /// feed the next switch's migration — so this never repaints.
    pub fn fold_inputs(&mut self, inputs: Vec<SinkInput>) -> bool {
        self.inputs = inputs;
        false
    }

    /// The control under a content-local point, on an available sink.
    /// Unavailable rows are greyed scenery: no target, no hover, no
    /// press.
    fn target_at(&self, point: Point, metrics: &PanelMetrics) -> Option<PanelTarget> {
        let row = metrics.row_at(point, self.sinks.len())?;
        let sink = &self.sinks[row];
        if !sink.available {
            return None;
        }
        let zone = if point.x >= metrics.mute_zone_left() { PanelZone::Mute } else { PanelZone::Row };
        Some(PanelTarget { sink: sink.name.clone(), zone })
    }

    /// Pointer motion: retargets the hover. Returns whether the
    /// highlight moved (= repaint).
    pub fn on_motion(&mut self, point: Point, metrics: &PanelMetrics) -> bool {
        let target = self.target_at(point, metrics);
        let changed = target != self.hover;
        self.hover = target;
        changed
    }

    /// The pointer left the panel: hover dies, and so does any
    /// half-finished press — releasing outside must not act.
    pub fn on_leave(&mut self) -> bool {
        let changed = self.hover.is_some() || self.pressed.is_some();
        self.hover = None;
        self.pressed = None;
        changed
    }

    /// Press: arms the control under the pointer. The action itself
    /// waits for the release — see [`AudioPanel::on_release`].
    pub fn on_press(&mut self, point: Point, metrics: &PanelMetrics) -> bool {
        let target = self.target_at(point, metrics);
        let changed = target != self.pressed;
        self.pressed = target;
        changed
    }

    /// The wheel over a row: that device's volume knob, the tile's
    /// gesture restated per-device. Any part of the row is the knob,
    /// mute square included — the same "the whole face is the knob"
    /// rule the tile follows.
    ///
    /// The target is *absolute*, computed from the sampled level rather
    /// than handed to `pactl` as a relative `+5%`, for two reasons:
    /// `pactl` has no `-l 1.0` of its own, so a relative step is the one
    /// way this panel could push a sink into overdrive; and an absolute
    /// set is idempotent, so a burst of notches that outruns the
    /// sampler asks for one level rather than compounding.
    ///
    /// Nothing to ask for — no row, no sampled level, or already at the
    /// end of the travel — is `None`, and never a repaint: the readout
    /// changes when the *sample* changes, exactly like the tile.
    pub fn on_scroll(&self, point: Point, delta: i32, metrics: &PanelMetrics) -> Option<PanelAction> {
        let target = self.target_at(point, metrics)?;
        let sink = self.sinks.iter().find(|sink| sink.name == target.sink)?;
        let current = sink.volume_percent?;
        let step = delta.signum() * VOLUME_STEP as i32;
        if step == 0 {
            return None;
        }
        // The ceiling yields to a sink already above it, so that a
        // scroll *down* on an overdriven sink still steps down rather
        // than snapping to 100 — but the step itself never raises the
        // ceiling, so a scroll *up* there asks for nothing.
        let ceiling = MAX_VOLUME.max(current) as i32;
        let next = (current as i32 + step).clamp(0, ceiling) as u32;
        (next != current).then_some(PanelAction::SetVolume { sink: target.sink, percent: next })
    }

    /// Release: fires the armed control, but only if the release is
    /// still on it — the classic button contract, so a slip off the
    /// row is a cancel, not a surprise device switch. A switch to the
    /// already-shown default is a no-op (nothing to ask for), every
    /// real switch starts the optimistic prediction immediately.
    ///
    /// The `bool` is "repaint": true whenever the pressed visual
    /// clears or the lamp jumps to the predicted row.
    pub fn on_release(&mut self, point: Point, metrics: &PanelMetrics) -> (Option<PanelAction>, bool) {
        let Some(pressed) = self.pressed.take() else { return (None, false) };
        let released = self.target_at(point, metrics);
        if released.as_ref() != Some(&pressed) {
            return (None, true);
        }
        let action = match pressed.zone {
            PanelZone::Mute => Some(PanelAction::ToggleMute(pressed.sink)),
            PanelZone::Row if self.shown_default() == Some(pressed.sink.as_str()) => None,
            PanelZone::Row => {
                self.prediction = Some(Prediction { name: pressed.sink.clone(), samples_left: PREDICTION_SAMPLES });
                Some(PanelAction::SwitchTo(pressed.sink))
            }
        };
        (action, true)
    }
}

#[cfg(test)]
mod tests;
