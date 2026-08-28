//! Sound instrument: the default PipeWire sink's volume and mute
//! state, sampled through `wpctl` and rendered by
//! `wm_theme::soundctl` on the `wm_theme::panel` LED screen.
//!
//! This module is the data half of the split `widgets::mod` describes:
//! it shells out, parses, and throttles; every pixel decision lives in
//! the renderer. The parse is a pure function over `wpctl`'s one-line
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
//! Every set is followed by an immediate resample so the tile answers
//! the click on the next repaint instead of a `SAMPLE_INTERVAL` later.
//! No `wpctl`, or no default sink, renders the SDK's dead screen and
//! turns clicks into no-ops until a sink appears.

use std::cell::RefCell;
use std::process::Command;
use std::time::Instant;

use wm_theme::{panel, soundctl, Theme};
use wm_theme_api::{DecorationBuffer, Point};

use super::{DockWidget, SAMPLE_INTERVAL};

/// One reading of the default sink. `PartialEq` is what lets `tick`
/// and `on_click` report "repaint needed" as a plain comparison.
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

/// One blocking `wpctl get-volume` round trip. `None` covers every
/// failure the same way — binary missing, PipeWire down, no default
/// sink — because the tile's answer to all of them is the dead screen.
fn sample_sink() -> Option<SinkState> {
    let out = Command::new("wpctl").args(["get-volume", "@DEFAULT_AUDIO_SINK@"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    parse_wpctl_volume(&text).map(|(volume, muted)| SinkState { volume, muted })
}

pub struct SoundWidget {
    last_sample: Instant,
    /// `None` = no usable sink; renders the dead tile and ignores
    /// clicks until a sample succeeds again.
    state: Option<SinkState>,
    font_system: RefCell<cosmic_text::FontSystem>,
    swash_cache: RefCell<cosmic_text::SwashCache>,
}

impl SoundWidget {
    pub fn new() -> Self {
        Self {
            // Backdated so the first tick samples immediately instead
            // of showing the dead screen for a full interval.
            last_sample: Instant::now() - SAMPLE_INTERVAL,
            state: None,
            font_system: RefCell::new(cosmic_text::FontSystem::new()),
            swash_cache: RefCell::new(cosmic_text::SwashCache::new()),
        }
    }

    /// Resamples now and resets the throttle clock — shared by the
    /// periodic tick and the after-a-set refresh, so a click can never
    /// double-pay the sampling cost within one interval.
    fn resample(&mut self) -> bool {
        self.last_sample = Instant::now();
        let fresh = sample_sink();
        let changed = fresh != self.state;
        self.state = fresh;
        changed
    }
}

impl Default for SoundWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl DockWidget for SoundWidget {
    fn tick(&mut self) -> bool {
        if self.last_sample.elapsed() < SAMPLE_INTERVAL {
            return false;
        }
        self.resample()
    }

    fn render(&self, theme: &Theme, tile: u32) -> DecorationBuffer {
        let mut font_system = self.font_system.borrow_mut();
        let mut swash_cache = self.swash_cache.borrow_mut();
        match self.state {
            Some(s) => soundctl::render_soundctl_tile(theme, &mut font_system, &mut swash_cache, tile, s.volume, s.muted),
            None => panel::render_dead_tile(theme, &mut font_system, &mut swash_cache, tile, "SND"),
        }
    }

    fn on_click(&mut self, local: Point, tile: u32) -> bool {
        // Dead screen, dead controls: without a sink the zones would
        // only shout into a missing mixer.
        if self.state.is_none() {
            return false;
        }
        let args = zone_command(soundctl::zone_at(local, tile));
        let _ = Command::new("wpctl").args(args).output();
        // Resample immediately (success or not): the sink is the
        // authority on what the click did, and a failed set that lost
        // the sink should drop to the dead screen now, not next tick.
        self.resample()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soundctl::SoundZone;

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
}
