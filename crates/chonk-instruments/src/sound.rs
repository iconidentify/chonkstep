//! Sound instrument: the default PipeWire sink's volume and mute
//! state, sampled through `wpctl` and rendered by
//! `wm_theme::soundctl` on the `wm_theme::panel` LED screen.
//!
//! This module is the data half of the split `widgets::mod` describes:
//! it declares, parses, and decides; every pixel decision lives in the
//! renderer. The parse is a pure function over `wpctl`'s one-line
//! output so it can be fixture-tested without an audio stack.
//!
//! Control zones (tile-local, y from the tile's top edge; the exact
//! boundaries are `wm_theme::soundctl::zone_at`, derived from the same
//! geometry the renderer draws):
//!
//! ```text
//! +----------------+
//! |  [8][5][5]     |   upper band (digits + top half of the slat
//! |  ============  |   stack): louder — set-volume 5%+, hard-capped
//! |  ============  |   at 100% with wpctl's own -l 1.0 limit
//! |----------------|
//! |  ============  |   lower half of the stack: softer — 5%-
//! +----------------+
//! | VOL      spkr> |   label strip at the base (the speaker mark):
//! +----------------+   mute toggle
//! ```
//!
//! The wheel is the fourth control: a scroll anywhere on the tile is
//! the volume knob — up is louder, down is softer, one notch per 5%
//! step through the same argv the click zones use. The shell already
//! replays a gesture as discrete ±1 notches (capped at 32 per report);
//! [`MAX_SCROLL_NOTCHES`] re-caps a single event's magnitude here so a
//! backend that ever hands a widget a raw multi-notch delta cannot buy
//! an unbounded run of `wpctl` invocations with one wheel report.
//!
//! Every set carries an immediate resample so the tile answers the
//! click a moment after the command lands, instead of a
//! `SAMPLE_INTERVAL` later.
//! No `wpctl`, or no default sink, renders the SDK's dead screen and
//! turns clicks into no-ops until a sink appears.
//!
//! Neither the sample nor the set runs here. The reading is a
//! [`Source::Command`] and the set is an [`Effect::Run`] with the
//! resample hung off its completion, so both `wpctl` invocations happen
//! on dock-owned threads. This widget was already careful about that —
//! it spawned its own — and the migration is worth naming for exactly
//! that reason: being careful was the part that did not survive contact
//! with the next widget written. Now the trait does not offer the
//! mistake.
//!
//! # The panel behind the tile
//!
//! The tile is one sink's volume; the panel ([`crate::audio_panel`]) is
//! every sink, with the click that switches the desktop's output. Its
//! data comes from three more sources, all `pactl` — the only tool in
//! the stack with machine-readable output — and all folded here in
//! `update` beside the tile's own reading, so an open panel is exactly
//! as live as the face it unfolded from.
//!
//! ```text
//! wpctl get-volume @DEFAULT_AUDIO_SINK@   1s   the tile
//! pactl -f json list sinks                1s   the rows
//! pactl get-default-sink                  1s   the lamp (and the
//!                                              prediction's deadline)
//! pactl -f json list sink-inputs          2s   the switch's migration
//! ```
//!
//! Nothing here samples only-while-open: the SDK has no panel-scoped
//! source, so the three `pactl` readings are paid whether or not
//! anything is looking. They are ~20-25ms each on a worker thread. The
//! stream list, which no pixel depends on, gets the slower interval.
//!
//! ## One reaction, several commands
//!
//! [`PanelReaction`] carries at most one [`Effect`], and a device
//! switch is `set-default-sink` *plus* one `pactl move-sink-input` per
//! playing stream — `pactl` takes one command per invocation, so the
//! recipe cannot be folded into a single argv. Until the reaction can
//! carry a list, the extra commands wait in [`SoundWidget::pending`]
//! and one drains ahead of each subsequent panel event (see
//! [`SoundWidget::react`]). The set-default always goes first and is
//! the half that carries the confirming resample, so the visible answer
//! is never the one that waits.

use std::cell::Cell;
use std::collections::VecDeque;
use std::time::Duration;

use wm_theme::{panel, soundctl, Theme};
use wm_theme_api::DecorationBuffer;

use chonk_dock_widget::{
    DockInput, DockWidget, Effect, PanelCtx, PanelEvent, PanelFrame, PanelReaction, PanelSpec, Samples, Source,
    SourceId, SAMPLE_INTERVAL,
};

use crate::audio_panel::{self, render::render_audio_panel, AudioPanel, PanelAction, PanelMetrics};

/// One reading of the default sink. `PartialEq` is what lets `update`
/// report "repaint needed" as a plain comparison.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SinkState {
    volume: f32,
    muted: bool,
}

/// Parses `wpctl get-volume` output — `"Volume: 0.45"` or
/// `"Volume: 0.45 [MUTED]"` — into `(volume, muted)`. `None` for
/// anything else (an error line, an empty read, a future format
/// change), which the widget treats as "no sink".
fn parse_wpctl_volume(output: &str) -> Option<(f32, bool)> {
    let rest = output.trim().strip_prefix("Volume:")?.trim();
    let volume: f32 = rest.split_whitespace().next()?.parse().ok()?;
    if !volume.is_finite() || volume < 0.0 {
        return None;
    }
    Some((volume, rest.ends_with("[MUTED]")))
}

/// The `wpctl` argument list for a zone's action. Louder carries
/// wpctl's own `-l 1.0` limit so repeated clicks pin at 100% instead
/// of climbing into overdrive; softer needs no floor (wpctl stops at
/// zero on its own).
fn zone_command(zone: soundctl::SoundZone) -> &'static [&'static str] {
    match zone {
        soundctl::SoundZone::Louder => &["set-volume", "-l", "1.0", "@DEFAULT_AUDIO_SINK@", "5%+"],
        soundctl::SoundZone::Softer => &["set-volume", "@DEFAULT_AUDIO_SINK@", "5%-"],
        soundctl::SoundZone::MuteToggle => &["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"],
    }
}

/// How many notches a single [`DockInput::Scroll`] event is honored
/// for. The shell delivers one event per notch (`delta` = ±1) and caps
/// a gesture at 32, so in practice this never binds; it exists so the
/// widget's own contract does not depend on the delivery convention
/// staying that shape. Eight notches is 40% — a full flick of a
/// physical wheel — and anything claiming more in one report is a
/// backend artifact, not a hand.
const MAX_SCROLL_NOTCHES: u32 = 8;

/// The volume zone a scroll notch drives: up is louder. Zero is no
/// notch at all — an axis report with no travel asks for nothing.
fn scroll_zone(delta: i32) -> Option<soundctl::SoundZone> {
    match delta.signum() {
        1 => Some(soundctl::SoundZone::Louder),
        -1 => Some(soundctl::SoundZone::Softer),
        _ => None,
    }
}

/// The `wpctl` query the sampler runs. `None` from its parse covers
/// every failure the same way — binary missing, PipeWire down, no
/// default sink — because the tile's answer to all of them is the dead
/// screen.
///
/// Measured at 22-32ms per call, which is under a frame at 30Hz but was
/// still paid on the compositor's repaint path once a second, and is
/// still unbounded if PipeWire ever wedges.
fn sink_args() -> Vec<String> {
    args(&["get-volume", "@DEFAULT_AUDIO_SINK@"])
}

/// A `&'static str` argv as the owned one [`Source`] and [`Effect`]
/// take. The program name stays compile-time; only the arguments are
/// owned, and the only ones that are not literals here are sink names
/// the panel read out of `pactl` itself.
fn args(argv: &[&str]) -> Vec<String> {
    argv.iter().map(|arg| (*arg).to_string()).collect()
}

/// How often the stream list is re-read. Slower than [`SAMPLE_INTERVAL`]
/// on purpose: no pixel depends on it — it is only the migration list a
/// device switch consults, and a switch happens seconds after the panel
/// opened, not in the same frame. Halving that reading's cost costs
/// nothing anyone can see.
const STREAM_INTERVAL: Duration = Duration::from_millis(2000);

pub struct SoundWidget {
    sink: SourceId,
    /// `pactl --format=json list sinks` — the panel's rows.
    sinks: SourceId,
    /// `pactl get-default-sink` — the lamp, and the authority the
    /// optimistic switch is reconciled against.
    default_sink: SourceId,
    /// `pactl --format=json list sink-inputs` — the switch's migration
    /// list. Invisible; sampled for the next click, not for the face.
    streams: SourceId,
    /// `None` = no usable sink; renders the dead tile and ignores
    /// clicks until a sample succeeds again.
    state: Option<SinkState>,
    /// The panel's own state — rows, hover, the armed press, the
    /// optimistic default. Folded here, drawn by
    /// [`render_audio_panel`], and asked about panel-local points.
    panel: AudioPanel,
    /// Whether `pactl` has ever answered the sink query. The panel is
    /// offered only once it has: with no `pactl` there is no panel to
    /// open, and the tile keeps working on `wpctl` alone.
    devices_known: bool,
    /// Set when a fold moved something the panel draws, taken by
    /// [`DockWidget::panel_tick`] — the "one boolean per pass, one
    /// repaint per actual change" shape the trait asks for.
    panel_dirty: bool,
    /// The tile edge the dock last rendered at, which is also the
    /// panel's scale yardstick. A `Cell` because
    /// [`DockWidget::render`] takes `&self` and
    /// [`DockWidget::panel_spec`] — which needs the number, one gesture
    /// later — does too.
    tile: Cell<u32>,
    /// The geometry of the last grant. Rebuilt from the frame in
    /// [`DockWidget::render_panel`], which the shell always calls
    /// before it delivers a pointer event, so the hit-test measures the
    /// panel that is actually on screen.
    metrics: PanelMetrics,
    /// Commands an action produced that the one-effect
    /// [`PanelReaction`] could not carry — see the module doc's "One
    /// reaction, several commands". Drained oldest-first, one per
    /// subsequent panel event.
    pending: VecDeque<Effect>,
}

/// The tile edge assumed until the dock has rendered once. Only the
/// panel's *requested* size depends on it, and the request is a request.
const ASSUMED_TILE: u32 = 56;

impl SoundWidget {
    pub fn new() -> Self {
        Self {
            sink: SourceId::UNBOUND,
            sinks: SourceId::UNBOUND,
            default_sink: SourceId::UNBOUND,
            streams: SourceId::UNBOUND,
            state: None,
            panel: AudioPanel::new(),
            devices_known: false,
            panel_dirty: false,
            tile: Cell::new(ASSUMED_TILE),
            metrics: PanelMetrics::granted(ASSUMED_TILE, 0, 0, 0),
            pending: VecDeque::new(),
        }
    }

    /// One zone action as the effect it becomes: run `wpctl`, then
    /// resample the sink the command just changed. Shared by the click
    /// zones and the wheel so both controls stay one argv table.
    fn zone_effect(&self, zone: soundctl::SoundZone) -> Effect {
        Effect::Run {
            program: "wpctl",
            args: args(zone_command(zone)),
            then: Some(self.sink),
        }
    }

    /// The source whose next reading proves what a panel action did.
    /// A switch is answered by `get-default-sink`; a mute or a level is
    /// answered by the sink list, which is where both are drawn from.
    fn confirms(&self, action: &PanelAction) -> SourceId {
        match action {
            PanelAction::SwitchTo(_) => self.default_sink,
            PanelAction::ToggleMute(_) | PanelAction::SetVolume { .. } => self.sinks,
        }
    }

    /// Queues an action's commands and answers the event that caused
    /// it. The first command leaves immediately as this event's
    /// reaction; any remainder waits for the next one.
    fn act(&mut self, action: &PanelAction, repaint: bool) -> PanelReaction {
        let confirm = Some(self.confirms(action));
        self.pending.extend(audio_panel::action_effects(action, self.panel.inputs(), confirm));
        self.react(repaint)
    }

    /// Turns "the pixels moved" into a reaction, draining one queued
    /// command ahead of it when there is one. A repaint displaced that
    /// way is not lost: [`DockWidget::panel_tick`] runs every
    /// event-loop pass an open panel has and picks it up on the next
    /// one.
    fn react(&mut self, repaint: bool) -> PanelReaction {
        match self.pending.pop_front() {
            Some(effect) => {
                self.panel_dirty |= repaint;
                PanelReaction::Run(effect)
            }
            None if repaint => PanelReaction::Repaint,
            None => PanelReaction::None,
        }
    }
}

impl Default for SoundWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl DockWidget for SoundWidget {
    fn name(&self) -> &str {
        "SND"
    }

    fn sources(&self) -> Vec<Source> {
        vec![
            Source::Command { program: "wpctl", args: sink_args(), interval: SAMPLE_INTERVAL },
            Source::Command { program: "pactl", args: args(&["--format=json", "list", "sinks"]), interval: SAMPLE_INTERVAL },
            Source::Command { program: "pactl", args: args(&["get-default-sink"]), interval: SAMPLE_INTERVAL },
            Source::Command {
                program: "pactl",
                args: args(&["--format=json", "list", "sink-inputs"]),
                interval: STREAM_INTERVAL,
            },
        ]
    }

    fn bind(&mut self, ids: &[SourceId]) {
        let id = |index: usize| ids.get(index).copied().unwrap_or(SourceId::UNBOUND);
        self.sink = id(0);
        self.sinks = id(1);
        self.default_sink = id(2);
        self.streams = id(3);
    }

    fn update(&mut self, samples: &Samples) -> bool {
        // Before the first run lands there is nothing to say, and
        // overwriting a good reading with `None` would flash the dead
        // tile on every startup. Only a completed run changes the face.
        let mut face_changed = false;
        if samples.fresh(self.sink) {
            let reading =
                samples.text(self.sink).and_then(parse_wpctl_volume).map(|(volume, muted)| SinkState { volume, muted });
            face_changed = reading != self.state;
            self.state = reading;
        }

        // The panel's three readings fold the same way and report their
        // change through `panel_dirty` instead: they move no tile pixel,
        // and an open panel asks for them in `panel_tick`.
        if samples.fresh(self.sinks) {
            let reading = samples.text(self.sinks).and_then(audio_panel::parse_sinks);
            self.devices_known |= reading.is_some();
            self.panel_dirty |= self.panel.fold_sinks(reading);
        }
        if samples.fresh(self.default_sink) {
            let reading = samples.text(self.default_sink).and_then(audio_panel::parse_default_sink);
            self.panel_dirty |= self.panel.fold_default(reading);
        }
        if samples.fresh(self.streams) {
            let streams = samples.text(self.streams).map(audio_panel::parse_sink_inputs).unwrap_or_default();
            self.panel_dirty |= self.panel.fold_inputs(streams);
        }

        face_changed
    }

    fn render(&self, theme: &Theme, tile: u32, fonts: &mut cosmic_text::FontSystem, swash: &mut cosmic_text::SwashCache) -> DecorationBuffer {
        self.tile.set(tile);
        match self.state {
            Some(s) => soundctl::render_soundctl_tile(theme, fonts, swash, tile, s.volume, s.muted),
            None => panel::render_dead_tile(theme, fonts, swash, tile, "SND"),
        }
    }

    fn on_input(&mut self, input: DockInput, tile: u32) -> Vec<Effect> {
        // Dead screen, dead controls: without a sink the zones would
        // only shout into a missing mixer.
        if self.state.is_none() {
            return Vec::new();
        }
        // No `Repaint` from either control: the pixels do not change
        // here. The set is a request, and the sink stays the authority
        // on what the input did — `then` just asks for that answer as
        // soon as the command lands, rather than at the next interval.
        // A tile that drew the volume it *asked for* would lie for a
        // second every time something else moved the mixer underneath
        // it.
        match input {
            DockInput::Press { local, .. } => vec![self.zone_effect(soundctl::zone_at(local, tile))],
            // The wheel ignores where on the tile it landed: the whole
            // face is the knob. One `wpctl` step per notch keeps the
            // wheel and the click zones the same 5% quantum.
            DockInput::Scroll { delta, .. } => match scroll_zone(delta) {
                Some(zone) => {
                    let notches = delta.unsigned_abs().min(MAX_SCROLL_NOTCHES);
                    (0..notches).map(|_| self.zone_effect(zone)).collect()
                }
                None => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    // -----------------------------------------------------------------
    // The panel: every sink, and the click that switches the desktop's
    // output. See `crate::audio_panel` for the data model and the
    // switch recipe.
    // -----------------------------------------------------------------

    /// No `pactl`, no panel: the open gesture does nothing until the
    /// sink query has answered at least once, and the tile goes on
    /// working from `wpctl` alone. An *empty* answer still opens — a
    /// machine with a mixer and no outputs has something to say, and
    /// the panel says it.
    fn panel_spec(&self, tile: u32) -> Option<PanelSpec> {
        if !self.devices_known {
            return None;
        }
        let (width, height) = PanelMetrics::request(tile, self.panel.sinks().len());
        Some(PanelSpec::new(width, height))
    }

    fn render_panel(&mut self, frame: &mut PanelFrame, ctx: &mut PanelCtx<'_>) {
        // The grant is the geometry — for the pixels and, from here on,
        // for the hit-test. A clamped grant is a shorter panel, not a
        // panel drawn against a size nobody agreed to.
        self.metrics = PanelMetrics::granted(ctx.tile, frame.width(), frame.height(), self.panel.sinks().len());
        let buffer = render_audio_panel(ctx.theme, ctx.fonts, ctx.swash, &self.metrics, &self.panel);
        // Rendered at exactly the granted size, so this cannot refuse;
        // if it ever did, the frame keeps its last good pixels, which
        // is the right answer to a stale grant anyway.
        let _ = frame.adopt(buffer);
    }

    fn panel_input(&mut self, event: PanelEvent, _tile: u32) -> PanelReaction {
        // The hit-test is against `metrics`, which was built from the
        // *granted* size in `render_panel` — a truer yardstick than the
        // tile edge, since a clamped grant is a shorter panel.
        let metrics = self.metrics;
        match event {
            PanelEvent::Motion { local } => {
                let moved = self.panel.on_motion(local, &metrics);
                self.react(moved)
            }
            // A crossing is not a position: the hover the pointer
            // brought in arrives as the `Motion` right behind it.
            PanelEvent::Enter => self.react(false),
            PanelEvent::Leave => {
                let cleared = self.panel.on_leave();
                self.react(cleared)
            }
            PanelEvent::LeftPress { local } => {
                let armed = self.panel.on_press(local, &metrics);
                self.react(armed)
            }
            PanelEvent::LeftRelease { local } => match self.panel.on_release(local, &metrics) {
                (Some(action), repaint) => self.act(&action, repaint),
                (None, repaint) => self.react(repaint),
            },
            // One notch per event, whatever the report claims: the
            // action is an absolute level, so the shell's own
            // one-event-per-notch replay is the only thing that decides
            // how far the knob turns.
            PanelEvent::Scroll { local, delta } => match self.panel.on_scroll(local, delta, &metrics) {
                Some(action) => self.act(&action, false),
                None => self.react(false),
            },
        }
    }

    fn panel_tick(&mut self, _now: std::time::Instant) -> bool {
        std::mem::take(&mut self.panel_dirty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chonk_dock_widget::SampleBench;
    use soundctl::SoundZone;
    use chonk_dock_widget::MouseButton;
    use wm_theme_api::Point;

    #[test]
    fn parses_the_documented_wpctl_formats() {
        assert_eq!(parse_wpctl_volume("Volume: 0.45\n"), Some((0.45, false)));
        assert_eq!(parse_wpctl_volume("Volume: 0.45 [MUTED]\n"), Some((0.45, true)));
        assert_eq!(parse_wpctl_volume("Volume: 1.00"), Some((1.0, false)));
        assert_eq!(parse_wpctl_volume("Volume: 0.00"), Some((0.0, false)));
        assert_eq!(parse_wpctl_volume("Volume: 1.20"), Some((1.2, false)), "overdrive passes through");
    }

    #[test]
    fn rejects_everything_that_is_not_a_volume_line() {
        for bad in ["", "garbage", "Volume:", "Volume: loud", "Volume: -0.2", "Volume: inf", "Error: no default sink"] {
            assert_eq!(parse_wpctl_volume(bad), None, "{bad:?} should not parse");
        }
    }

    #[test]
    fn zone_commands_cap_louder_and_only_louder() {
        assert!(zone_command(SoundZone::Louder).windows(2).any(|w| w == ["-l", "1.0"]));
        assert!(zone_command(SoundZone::Louder).contains(&"5%+"));
        assert!(zone_command(SoundZone::Softer).contains(&"5%-"));
        assert!(!zone_command(SoundZone::Softer).contains(&"-l"));
        assert_eq!(zone_command(SoundZone::MuteToggle), ["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"]);
    }

    /// One `wpctl` line in, one sink state out — and the tile only
    /// repaints when the reading actually moved.
    #[test]
    fn update_folds_a_wpctl_line_into_the_sink_state() {
        let mut bench = SampleBench::new();
        let id = bench.text("Volume: 0.45\n");
        let mut widget = SoundWidget::new();
        widget.bind(&[id]);

        assert!(widget.update(&bench.samples()));
        assert_eq!(widget.state, Some(SinkState { volume: 0.45, muted: false }));

        bench.all_stale();
        assert!(!widget.update(&bench.samples()), "a stale pass folds nothing");

        bench.set_text(id, "Volume: 0.45 [MUTED]\n");
        assert!(widget.update(&bench.samples()));
        assert_eq!(widget.state, Some(SinkState { volume: 0.45, muted: true }));

        bench.set_text(id, "Volume: 0.45 [MUTED]\n");
        assert!(!widget.update(&bench.samples()), "the same reading again is not a repaint");
    }

    /// No `wpctl` on the machine, or no default sink: the tile shows
    /// the dead screen and its zones stop answering. A control that
    /// fires into a mixer that is not there is worse than no control.
    #[test]
    fn without_a_sink_the_face_is_dead_and_the_zones_are_inert() {
        let mut bench = SampleBench::new();
        let id = bench.unusable();
        let mut widget = SoundWidget::new();
        widget.bind(&[id]);
        widget.update(&bench.samples());

        assert_eq!(widget.state, None);
        let press = DockInput::Press { local: Point::new(28, 8), button: MouseButton::Left };
        assert!(widget.on_input(press, 56).is_empty());
    }

    /// A press in the louder zone emits exactly one effect: run
    /// `wpctl`, then resample the sink it just changed. Nothing else —
    /// in particular no `Repaint`, because the widget has not learned
    /// anything yet.
    #[test]
    fn a_press_emits_one_run_effect_pointed_back_at_its_own_sampler() {
        let mut bench = SampleBench::new();
        let id = bench.text("Volume: 0.40\n");
        let mut widget = SoundWidget::new();
        widget.bind(&[id]);
        widget.update(&bench.samples());

        // The top of the tile is the louder zone; `zone_at` owns the
        // exact geometry and is tested against the renderer's.
        let press = DockInput::Press { local: Point::new(28, 4), button: MouseButton::Left };
        let effects = widget.on_input(press, 56);
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::Run { program, args, then } => {
                assert_eq!(*program, "wpctl");
                assert_eq!(args, &zone_command(SoundZone::Louder).iter().map(|a| a.to_string()).collect::<Vec<_>>());
                assert_eq!(*then, Some(id), "the set must nudge the sampler that will confirm it");
            }
            _ => panic!("a volume zone press must be a Run effect"),
        }

        // A release is the same click's other edge and must not fire a
        // second `wpctl`.
        let release = DockInput::Release { local: Point::new(28, 4), button: MouseButton::Left };
        assert!(widget.on_input(release, 56).is_empty());
    }

    /// A live widget with a bound sampler, for the scroll tests.
    fn live_widget() -> (SoundWidget, SourceId) {
        let mut bench = SampleBench::new();
        let id = bench.text("Volume: 0.40\n");
        let mut widget = SoundWidget::new();
        widget.bind(&[id]);
        widget.update(&bench.samples());
        (widget, id)
    }

    /// One notch, one `wpctl` step — up is louder, down is softer, and
    /// both carry the exact argv the click zones use, pointed back at
    /// the widget's own sampler.
    #[test]
    fn a_scroll_notch_maps_to_the_matching_zone_command() {
        let (mut widget, id) = live_widget();
        for (delta, zone) in [(1, SoundZone::Louder), (-1, SoundZone::Softer)] {
            let effects = widget.on_input(DockInput::Scroll { local: Point::new(28, 50), delta }, 56);
            assert_eq!(effects.len(), 1, "delta {delta}: one notch is one step");
            match &effects[0] {
                Effect::Run { program, args, then } => {
                    assert_eq!(*program, "wpctl");
                    assert_eq!(args, &zone_command(zone).iter().map(|a| a.to_string()).collect::<Vec<_>>());
                    assert_eq!(*then, Some(id), "the set must nudge the sampler that will confirm it");
                }
                _ => panic!("a scroll notch must be a Run effect"),
            }
        }
    }

    /// The wheel works anywhere on the face — including the mute strip,
    /// where a click means something else entirely.
    #[test]
    fn scroll_ignores_the_click_zone_map() {
        let (mut widget, _) = live_widget();
        for y in [0, 28, 55] {
            let effects = widget.on_input(DockInput::Scroll { local: Point::new(28, y), delta: 1 }, 56);
            assert_eq!(effects.len(), 1, "y {y}: the whole face is the knob");
            match &effects[0] {
                Effect::Run { args, .. } => assert!(args.contains(&"5%+".to_string()), "y {y}: up is louder everywhere"),
                _ => panic!("scroll must be a Run effect"),
            }
        }
    }

    /// A zero-travel axis report asks for nothing, and a single event
    /// claiming an implausible notch count is clamped rather than
    /// replayed in full.
    #[test]
    fn scroll_bursts_are_clamped_and_zero_is_ignored() {
        let (mut widget, _) = live_widget();
        assert!(widget.on_input(DockInput::Scroll { local: Point::new(28, 4), delta: 0 }, 56).is_empty());
        let burst = widget.on_input(DockInput::Scroll { local: Point::new(28, 4), delta: 1000 }, 56);
        assert_eq!(burst.len(), MAX_SCROLL_NOTCHES as usize);
        let burst = widget.on_input(DockInput::Scroll { local: Point::new(28, 4), delta: i32::MIN }, 56);
        assert_eq!(burst.len(), MAX_SCROLL_NOTCHES as usize, "i32::MIN must clamp, not overflow");
    }

    /// No sink, no knob: the dead tile's wheel is as inert as its
    /// zones.
    #[test]
    fn without_a_sink_the_wheel_is_inert_too() {
        let mut bench = SampleBench::new();
        let id = bench.unusable();
        let mut widget = SoundWidget::new();
        widget.bind(&[id]);
        widget.update(&bench.samples());
        assert!(widget.on_input(DockInput::Scroll { local: Point::new(28, 4), delta: 1 }, 56).is_empty());
    }

    // -----------------------------------------------------------------
    // The panel's wiring. What the panel *is* — the parse, the argv
    // table, the state machine, the face — is tested in
    // `audio_panel::tests`; these are about this widget being plumbed to
    // it correctly.
    // -----------------------------------------------------------------

    /// Two sinks, shaped like the real `pactl --format=json list sinks`
    /// and trimmed to the fields the fold reads: an available HDMI at
    /// 40%, and a RUNNING USB interface at 100%.
    const SINKS: &str = r#"[
      { "name": "hdmi", "description": "HDMI Out", "state": "SUSPENDED", "mute": false,
        "volume": { "front-left": { "value_percent": "40%" } },
        "ports": [ { "availability": "available" } ] },
      { "name": "usb", "description": "Volt 4", "state": "RUNNING", "mute": false,
        "volume": { "front-left": { "value_percent": "100%" } },
        "ports": [ { "availability": "available" } ] }
    ]"#;

    /// One real application stream on the USB sink, plus a filter
    /// chain's nameless one that a switch must leave where it is.
    const STREAMS: &str = r#"[
      { "index": 7, "properties": { "application.name": "Microsoft Edge" } },
      { "index": 8, "properties": { "media.name": "output_FL" } }
    ]"#;

    /// The four sources bound in declaration order, all fresh: the
    /// `wpctl` line for the tile, then the three `pactl` readings.
    struct Bench {
        bench: SampleBench,
        wpctl: SourceId,
        sinks: SourceId,
        default_sink: SourceId,
        streams: SourceId,
    }

    fn panel_widget() -> (SoundWidget, Bench) {
        let mut bench = SampleBench::new();
        let wpctl = bench.text("Volume: 1.00\n");
        let sinks = bench.text(SINKS);
        let default_sink = bench.text("usb\n");
        let streams = bench.text(STREAMS);
        let mut widget = SoundWidget::new();
        widget.bind(&[wpctl, sinks, default_sink, streams]);
        widget.update(&bench.samples());
        (widget, Bench { bench, wpctl, sinks, default_sink, streams })
    }

    /// The 56px metrics this widget's panel gets when the shell grants
    /// its request in full, and points on row 0 (the HDMI) and row 1's
    /// mute square. Established by taking the grant, exactly as the
    /// shell does, rather than by assuming one.
    fn grant(widget: &SoundWidget, rows: usize) -> (u32, u32) {
        let spec = widget.panel_spec(56).expect("the panel is on offer");
        assert_eq!((spec.width, spec.height), PanelMetrics::request(56, rows));
        (spec.width, spec.height)
    }

    fn open(widget: &mut SoundWidget, size: (u32, u32)) -> PanelFrame {
        let mut frame = PanelFrame::new(size.0, size.1);
        let theme = wm_theme::default_theme::nextstep_classic();
        let mut fonts = cosmic_text::FontSystem::new();
        let mut swash = cosmic_text::SwashCache::new();
        widget.render_panel(&mut frame, &mut PanelCtx { theme: &theme, tile: 56, fonts: &mut fonts, swash: &mut swash });
        frame
    }

    fn run_argv(reaction: &PanelReaction) -> (&'static str, Vec<String>, Option<SourceId>) {
        match reaction {
            PanelReaction::Run(Effect::Run { program, args, then }) => (*program, args.clone(), *then),
            _ => panic!("expected a Run reaction"),
        }
    }

    /// A point on row `i`'s body / mute square, for a panel granted its
    /// request at a 56px tile.
    fn row(i: i32) -> Point {
        Point::new(40, 4 + i * 34 + 10)
    }

    fn mute(i: i32) -> Point {
        Point::new(310, 4 + i * 34 + 10)
    }

    /// The open gesture does nothing until `pactl` has answered once —
    /// a desktop without it keeps a working sound tile and simply has
    /// no panel behind it. Once it has, the request is one row per
    /// device.
    #[test]
    fn the_panel_is_offered_only_once_pactl_has_answered() {
        let mut widget = SoundWidget::new();
        assert!(widget.panel_spec(56).is_none(), "nothing sampled, nothing to show");

        let mut bench = SampleBench::new();
        let wpctl = bench.text("Volume: 0.40\n");
        let sinks = bench.unusable();
        let default_sink = bench.unusable();
        let streams = bench.unusable();
        widget.bind(&[wpctl, sinks, default_sink, streams]);
        widget.update(&bench.samples());
        assert!(widget.state.is_some(), "the tile still works on wpctl alone");
        assert!(widget.panel_spec(56).is_none(), "no pactl, no panel");

        let (widget, _) = panel_widget();
        assert_eq!(widget.panel_spec(56).map(|s| (s.width, s.height)), Some(PanelMetrics::request(56, 2)));
    }

    /// A machine whose mixer answers with no outputs at all still gets a
    /// panel — one that says so. Silence about it would be
    /// indistinguishable from a broken gesture.
    #[test]
    fn a_mixer_with_no_outputs_still_opens() {
        let mut bench = SampleBench::new();
        let ids = [bench.text("Volume: 0.40\n"), bench.text("[]"), bench.missing(), bench.missing()];
        let mut widget = SoundWidget::new();
        widget.bind(&ids);
        widget.update(&bench.samples());
        assert_eq!(widget.panel_spec(56).map(|s| (s.width, s.height)), Some(PanelMetrics::request(56, 0)));
    }

    /// The three `pactl` readings fold in `update` beside the tile's,
    /// and report themselves through `panel_tick` rather than through
    /// the tile's repaint — they move no tile pixel.
    #[test]
    fn the_pactl_readings_fold_into_the_panel_and_tick_once_per_change() {
        let (mut widget, mut b) = panel_widget();
        assert_eq!(widget.panel.sinks().len(), 2);
        assert_eq!(widget.panel.shown_default(), Some("usb"));
        assert_eq!(widget.panel.inputs().len(), 2);
        assert!(widget.panel_tick(std::time::Instant::now()), "the first fold is a repaint");
        assert!(!widget.panel_tick(std::time::Instant::now()), "and it is taken, not left set");

        b.bench.all_stale();
        assert!(!widget.update(&b.bench.samples()), "a stale pass folds nothing");
        assert!(!widget.panel_tick(std::time::Instant::now()));

        b.bench.set_text(b.default_sink, "hdmi\n");
        widget.update(&b.bench.samples());
        assert_eq!(widget.panel.shown_default(), Some("hdmi"));
        assert!(widget.panel_tick(std::time::Instant::now()), "the lamp moved");

        // A fresh stream list is invisible: it only feeds the next
        // switch's migration.
        b.bench.all_stale();
        b.bench.set_text(b.streams, "[]");
        widget.update(&b.bench.samples());
        assert!(widget.panel.inputs().is_empty());
        assert!(!widget.panel_tick(std::time::Instant::now()), "streams move no pixels");

        // And the tile's own reading is still the tile's own business.
        b.bench.all_stale();
        b.bench.set_text(b.wpctl, "Volume: 0.20\n");
        assert!(widget.update(&b.bench.samples()), "the face changed");
        assert_eq!(widget.state, Some(SinkState { volume: 0.20, muted: false }));
    }

    /// The panel renders into the buffer it was handed, at exactly that
    /// size — including a grant the shell clamped, which is a shorter
    /// panel rather than a panel drawn against a size nobody agreed to.
    #[test]
    fn render_panel_fills_exactly_what_was_granted() {
        let (mut widget, _) = panel_widget();
        let asked = grant(&widget, 2);
        for size in [asked, (asked.0, asked.1 / 2), (240, 60), (500, 400)] {
            let frame = open(&mut widget, size);
            assert_eq!((frame.width(), frame.height()), size);
            assert_eq!(frame.buffer().pixels.len(), (size.0 * size.1 * 4) as usize);
            assert!(
                frame.buffer().pixels.chunks_exact(4).all(|px| px[3] == 255),
                "grant {size:?}: the panel fills its grant — no transparent hole"
            );
            // And the hit-test moved with it: the mute square tracks the
            // granted right edge, never the requested one.
            assert_eq!(widget.metrics.width, size.0);
            assert_eq!(widget.metrics.height, size.1);
        }
    }

    /// A row click is the switch recipe: `set-default-sink` first,
    /// carrying the resample of the reading that will confirm it, then
    /// one `move-sink-input` per real application stream. The reaction
    /// carries one effect, so the migration rides the next panel event
    /// — see the module doc.
    #[test]
    fn a_row_click_switches_and_migrates_the_playing_streams() {
        let (mut widget, b) = panel_widget();
        let size = grant(&widget, 2);
        open(&mut widget, size);

        assert!(matches!(widget.panel_input(PanelEvent::LeftPress { local: row(0) }, 56), PanelReaction::Repaint));
        let reaction = widget.panel_input(PanelEvent::LeftRelease { local: row(0) }, 56);
        let (program, args, then) = run_argv(&reaction);
        assert_eq!(program, "pactl");
        assert_eq!(args, ["set-default-sink", "hdmi"]);
        assert_eq!(then, Some(b.default_sink), "the default reading is what proves the switch");
        assert_eq!(widget.panel.shown_default(), Some("hdmi"), "the lamp jumps on the click, optimistically");

        // The next event drains the migration. Only the named stream
        // moves; the filter chain's nameless one stays put.
        let reaction = widget.panel_input(PanelEvent::Motion { local: row(1) }, 56);
        let (program, args, then) = run_argv(&reaction);
        assert_eq!(program, "pactl");
        assert_eq!(args, ["move-sink-input", "7", "hdmi"]);
        assert_eq!(then, None, "a migration changes nothing the panel draws");
        assert!(widget.pending.is_empty(), "one real stream, one move");

        // The repaint that event asked for is not lost — it is deferred
        // by exactly one pass, onto the tick.
        assert!(widget.panel_tick(std::time::Instant::now()));

        // And the panel is back to ordinary reactions.
        assert!(matches!(widget.panel_input(PanelEvent::Motion { local: row(0) }, 56), PanelReaction::Repaint));
    }

    /// A press that does not finish on what it started on asks for
    /// nothing at all — and neither does a click on the row that
    /// already is the default.
    #[test]
    fn a_slipped_click_and_a_click_on_the_default_ask_for_nothing() {
        let (mut widget, _) = panel_widget();
        let size = grant(&widget, 2);
        open(&mut widget, size);

        widget.panel_input(PanelEvent::LeftPress { local: row(0) }, 56);
        assert!(matches!(widget.panel_input(PanelEvent::LeftRelease { local: row(1) }, 56), PanelReaction::Repaint));
        assert!(widget.pending.is_empty(), "a slip queues nothing");

        // Row 1 is the USB sink, which is already the default.
        widget.panel_input(PanelEvent::LeftPress { local: row(1) }, 56);
        assert!(matches!(widget.panel_input(PanelEvent::LeftRelease { local: row(1) }, 56), PanelReaction::Repaint));
        assert!(widget.pending.is_empty());
    }

    /// The mute square is one command on one name, confirmed by the
    /// sink list it is drawn from — and it fits in one reaction, so
    /// nothing queues behind it.
    #[test]
    fn the_mute_square_is_one_command_confirmed_by_the_sink_list() {
        let (mut widget, b) = panel_widget();
        let size = grant(&widget, 2);
        open(&mut widget, size);

        widget.panel_input(PanelEvent::LeftPress { local: mute(1) }, 56);
        let reaction = widget.panel_input(PanelEvent::LeftRelease { local: mute(1) }, 56);
        let (program, args, then) = run_argv(&reaction);
        assert_eq!(program, "pactl");
        assert_eq!(args, ["set-sink-mute", "usb", "toggle"]);
        assert_eq!(then, Some(b.sinks));
        assert!(widget.pending.is_empty());
        assert_eq!(widget.panel.shown_default(), Some("usb"), "muting is not switching");
    }

    /// The wheel over a row is that device's knob, absolute and capped,
    /// confirmed by the sink list. Nothing to ask for is `None`, not a
    /// repaint: the readout follows the sample, not the gesture.
    #[test]
    fn the_panel_wheel_is_the_hovered_devices_knob() {
        let (mut widget, b) = panel_widget();
        let size = grant(&widget, 2);
        open(&mut widget, size);

        let reaction = widget.panel_input(PanelEvent::Scroll { local: row(0), delta: 1 }, 56);
        let (program, args, then) = run_argv(&reaction);
        assert_eq!(program, "pactl");
        assert_eq!(args, ["set-sink-volume", "hdmi", "45%"], "40% plus one 5% notch, absolutely");
        assert_eq!(then, Some(b.sinks));

        // Row 1 is at 100%: the knob is at its stop.
        assert!(matches!(widget.panel_input(PanelEvent::Scroll { local: row(1), delta: 1 }, 56), PanelReaction::None));
        let reaction = widget.panel_input(PanelEvent::Scroll { local: row(1), delta: -1 }, 56);
        assert_eq!(run_argv(&reaction).1, ["set-sink-volume", "usb", "95%"]);

        // Off the rows entirely: no knob.
        assert!(matches!(widget.panel_input(PanelEvent::Scroll { local: Point::new(40, 0), delta: 1 }, 56), PanelReaction::None));
    }

    /// Crossings are not positions, and leaving disarms: a release the
    /// pointer walked away from cannot fire a device switch on its way
    /// out.
    #[test]
    fn crossings_track_the_pointer_without_acting() {
        let (mut widget, _) = panel_widget();
        let size = grant(&widget, 2);
        open(&mut widget, size);

        assert!(matches!(widget.panel_input(PanelEvent::Enter, 56), PanelReaction::None));
        widget.panel_input(PanelEvent::LeftPress { local: row(0) }, 56);
        assert!(matches!(widget.panel_input(PanelEvent::Leave, 56), PanelReaction::Repaint));
        assert!(matches!(widget.panel_input(PanelEvent::LeftRelease { local: row(0) }, 56), PanelReaction::None));
        assert!(widget.pending.is_empty(), "an abandoned press switches nothing");
    }
}
