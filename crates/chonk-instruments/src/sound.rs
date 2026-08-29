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

use wm_theme::{panel, soundctl, Theme};
use wm_theme_api::DecorationBuffer;

use chonk_dock_widget::{DockInput, DockWidget, Effect, Samples, Source, SourceId, SAMPLE_INTERVAL};

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

/// The `wpctl` query the sampler runs. `None` from its parse covers
/// every failure the same way — binary missing, PipeWire down, no
/// default sink — because the tile's answer to all of them is the dead
/// screen.
///
/// Measured at 22-32ms per call, which is under a frame at 30Hz but was
/// still paid on the compositor's repaint path once a second, and is
/// still unbounded if PipeWire ever wedges.
fn sink_args() -> Vec<String> {
    ["get-volume", "@DEFAULT_AUDIO_SINK@"].iter().map(|arg| (*arg).to_string()).collect()
}

pub struct SoundWidget {
    sink: SourceId,
    /// `None` = no usable sink; renders the dead tile and ignores
    /// clicks until a sample succeeds again.
    state: Option<SinkState>,
}

impl SoundWidget {
    pub fn new() -> Self {
        Self { sink: SourceId::UNBOUND, state: None }
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
        vec![Source::Command { program: "wpctl", args: sink_args(), interval: SAMPLE_INTERVAL }]
    }

    fn bind(&mut self, ids: &[SourceId]) {
        self.sink = ids.first().copied().unwrap_or(SourceId::UNBOUND);
    }

    fn update(&mut self, samples: &Samples) -> bool {
        // Before the first run lands there is nothing to say, and
        // overwriting a good reading with `None` would flash the dead
        // tile on every startup. Only a completed run changes the face.
        if !samples.fresh(self.sink) {
            return false;
        }
        let reading = samples.text(self.sink).and_then(parse_wpctl_volume).map(|(volume, muted)| SinkState { volume, muted });
        let changed = reading != self.state;
        self.state = reading;
        changed
    }

    fn render(&self, theme: &Theme, tile: u32, fonts: &mut cosmic_text::FontSystem, swash: &mut cosmic_text::SwashCache) -> DecorationBuffer {
        match self.state {
            Some(s) => soundctl::render_soundctl_tile(theme, fonts, swash, tile, s.volume, s.muted),
            None => panel::render_dead_tile(theme, fonts, swash, tile, "SND"),
        }
    }

    fn on_input(&mut self, input: DockInput, tile: u32) -> Vec<Effect> {
        // Dead screen, dead controls: without a sink the zones would
        // only shout into a missing mixer.
        let DockInput::Press { local, .. } = input else { return Vec::new() };
        if self.state.is_none() {
            return Vec::new();
        }
        // No `Repaint`: the pixels do not change here. The set is a
        // request, and the sink stays the authority on what the click
        // did — `then` just asks for that answer as soon as the command
        // lands, rather than at the next interval. A tile that drew the
        // volume it *asked for* would lie for a second every time
        // something else moved the mixer underneath it.
        vec![Effect::Run {
            program: "wpctl",
            args: zone_command(soundctl::zone_at(local, tile)).iter().map(|arg| (*arg).to_string()).collect(),
            then: Some(self.sink),
        }]
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
}
