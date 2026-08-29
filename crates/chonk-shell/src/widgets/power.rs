//! Power instrument: battery capacity, charge state, AC presence, read
//! from `/sys/class/power_supply` and rendered by `wm_theme::power` —
//! this file is the data half of the split the widget SDK prescribes,
//! so everything here is interpretation, no drawing and (since Layer 3)
//! no walking of the directory either.
//!
//! The walk is a [`Source::Tree`]: the dock's sampler thread does the
//! `read_dir` and the up-to-four small reads per supply, and `update`
//! is handed the contents. `parse_supply` and `summarize` are unchanged
//! — the migration moved the four `read_to_string` calls off the
//! compositor's repaint thread and rewired what feeds them, nothing
//! more. Reading a battery's `capacity` can go out to an embedded
//! controller over I2C, which is precisely the kind of small, usually
//! instant read that is occasionally not.
//!
//! The interpretation degrades per-field on purpose: a supply directory
//! with an unreadable `capacity` still contributes its `status`, one
//! with a missing `type` is still classified by the battery-shaped
//! fields it does have, and only a machine exposing *nothing* falls
//! through to the renderer's dead screen. Desktops and VMs — a `Mains`
//! supply and no battery — get the deliberate "on line power" face
//! instead.

use std::path::PathBuf;
use std::time::Duration;

use wm_theme::power::{render_power_tile, ChargeState, PowerFace};
use wm_theme::Theme;
use wm_theme_api::DecorationBuffer;

use super::{DockWidget, Samples, Source, SourceId, TreeEntry};

/// Sampling interval — deliberately wider than the shared
/// `SAMPLE_INTERVAL`: battery percentage moves on the scale of minutes,
/// so a faster cadence would buy nothing and cost a wake-up per second
/// on a machine whose whole reason for having a battery gauge is that
/// it is running on the battery.
const POWER_SAMPLE_INTERVAL: Duration = Duration::from_secs(10);

/// Where the supplies live, and which of each supply's files the tile
/// reads. Positional: [`SupplyReading`] is built from these indices, so
/// the array and [`reading_from`] have to stay in step.
const SUPPLY_ROOT: &str = "/sys/class/power_supply";
const SUPPLY_FIELDS: &[&str] = &["type", "capacity", "status", "online"];

/// What a supply's `type` file declares it to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SupplyKind {
    Battery,
    /// Any external source: `Mains`, plus `UPS`/`USB`/`Wireless` — from
    /// this instrument's point of view they are all "power coming from
    /// outside", which is the only distinction the face draws.
    Mains,
    /// Missing or unrecognized `type` — kept, not discarded: the other
    /// fields may still identify it (see [`summarize`]).
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BatteryStatus {
    Charging,
    Discharging,
    /// On AC, held below the charge threshold — line-powered, so the
    /// face treats it like `Full`.
    NotCharging,
    Full,
    Unknown,
}

/// One `/sys/class/power_supply/<name>` entry, every field independently
/// optional — a missing file degrades that field alone.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SupplyReading {
    kind: SupplyKind,
    capacity: Option<u8>,
    status: Option<BatteryStatus>,
    online: Option<bool>,
}

/// Pure parser over one supply's raw file contents, `None` per missing
/// file. Unparseable values degrade to `None`/`Unknown` the same way
/// missing ones do — sysfs occasionally serves empty or garbage reads
/// mid-update and that must never blank the whole instrument.
fn parse_supply(kind: Option<&str>, capacity: Option<&str>, status: Option<&str>, online: Option<&str>) -> SupplyReading {
    let kind = match kind.map(str::trim) {
        Some("Battery") => SupplyKind::Battery,
        Some("Mains") | Some("UPS") | Some("USB") | Some("Wireless") => SupplyKind::Mains,
        _ => SupplyKind::Other,
    };
    let capacity = capacity.and_then(|c| c.trim().parse::<u8>().ok()).map(|c| c.min(100));
    let status = status.map(|s| match s.trim() {
        "Charging" => BatteryStatus::Charging,
        "Discharging" => BatteryStatus::Discharging,
        "Not charging" => BatteryStatus::NotCharging,
        "Full" => BatteryStatus::Full,
        _ => BatteryStatus::Unknown,
    });
    let online = online.and_then(|o| match o.trim() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    });
    SupplyReading { kind, capacity, status, online }
}

/// Boils every supply down to the one [`PowerFace`] the renderer takes.
/// Order-insensitive on purpose — `read_dir` promises no ordering, and
/// the face must not flicker between two answers across samples.
fn summarize(supplies: &[SupplyReading]) -> PowerFace {
    // Battery-shaped evidence beats a missing `type` file: a capacity
    // percentage or a charge status only ever comes from a battery.
    let battery_ish = |s: &&SupplyReading| {
        s.kind == SupplyKind::Battery
            || (s.kind == SupplyKind::Other
                && (s.capacity.is_some()
                    || matches!(
                        s.status,
                        Some(BatteryStatus::Charging | BatteryStatus::Discharging | BatteryStatus::NotCharging | BatteryStatus::Full)
                    )))
    };
    let batteries: Vec<&SupplyReading> = supplies.iter().filter(battery_ish).collect();

    if batteries.is_empty() {
        // A Mains entry with no battery is a desktop or VM: line power
        // is a fact worth a face. Anything else readable-but-empty
        // genuinely tells us nothing about power.
        return if supplies.iter().any(|s| s.kind == SupplyKind::Mains) { PowerFace::AcOnly } else { PowerFace::NoInfo };
    }

    // Multi-battery machines (classic ThinkPads) drain one cell at a
    // time; the honest single reading is the mean of the cells that
    // report one. Weighting by each cell's design capacity would be
    // more precise but needs fields many batteries don't expose.
    let known: Vec<u32> = batteries.iter().filter_map(|s| s.capacity.map(u32::from)).collect();
    let capacity = if known.is_empty() { None } else { Some((known.iter().sum::<u32>() / known.len() as u32) as u8) };

    // `online` files missing counts as "no contrary evidence": a Mains
    // supply that exists at all is overwhelmingly likely attached.
    let ac_online = supplies.iter().any(|s| s.kind == SupplyKind::Mains && s.online != Some(false));
    let any = |st: BatteryStatus| batteries.iter().any(|s| s.status == Some(st));
    // Priority: any cell taking charge means the machine is charging;
    // any cell draining (and none charging) means it is running on
    // battery; only an all-full/held pack is "full on AC". Statuses
    // absent entirely fall back to what the AC line says.
    let state = if any(BatteryStatus::Charging) {
        ChargeState::Charging
    } else if any(BatteryStatus::Discharging) {
        ChargeState::Discharging
    } else if any(BatteryStatus::Full) || any(BatteryStatus::NotCharging) || ac_online {
        // Two different routes to the same answer, folded into one arm
        // because they are one answer: a pack reporting Full/NotCharging
        // says so itself, and a pack reporting no status at all while
        // the AC line is up is the fallback for the same conclusion.
        ChargeState::Full
    } else {
        ChargeState::Discharging
    };
    PowerFace::Battery { capacity, state }
}

/// One sampled supply directory to one [`SupplyReading`] — the whole
/// of what replaced this module's `read_dir`. Pure, because the reads
/// already happened on a sampler thread.
fn reading_from(entry: &TreeEntry) -> SupplyReading {
    parse_supply(entry.file(0), entry.file(1), entry.file(2), entry.file(3))
}

pub struct PowerWidget {
    supplies: SourceId,
    face: PowerFace,
}

impl PowerWidget {
    pub fn new() -> Self {
        // `NoInfo` until the first sample lands: the dead screen is the
        // honest face for "has not looked yet", and the sampler's first
        // run happens immediately rather than an interval in.
        Self { supplies: SourceId::UNBOUND, face: PowerFace::NoInfo }
    }
}

impl Default for PowerWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl DockWidget for PowerWidget {
    fn name(&self) -> &'static str {
        "PWR"
    }

    fn sources(&self) -> Vec<Source> {
        vec![Source::Tree {
            root: PathBuf::from(SUPPLY_ROOT),
            files: SUPPLY_FIELDS,
            // No subdirectory tells this tile anything: a supply either
            // declares its `type` or is classified by the fields it
            // exposes.
            dirs: &[],
            interval: POWER_SAMPLE_INTERVAL,
        }]
    }

    fn bind(&mut self, ids: &[SourceId]) {
        self.supplies = ids.first().copied().unwrap_or(SourceId::UNBOUND);
    }

    fn update(&mut self, samples: &Samples) -> bool {
        if !samples.fresh(self.supplies) {
            return false;
        }
        let supplies: Vec<SupplyReading> = samples.tree(self.supplies).iter().map(reading_from).collect();
        let face = summarize(&supplies);
        if face == self.face {
            return false;
        }
        self.face = face;
        true
    }

    fn render(&self, theme: &Theme, tile: u32, fonts: &mut cosmic_text::FontSystem, swash: &mut cosmic_text::SwashCache) -> DecorationBuffer {
        render_power_tile(theme, fonts, swash, tile, self.face)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::sampling::SampleBench;

    /// One `/sys/class/power_supply/<name>` directory as the sampler
    /// would have delivered it, positional against `SUPPLY_FIELDS`.
    fn supply(name: &str, fields: [Option<&str>; 4]) -> TreeEntry {
        TreeEntry {
            name: name.to_string(),
            files: fields.iter().map(|f| f.map(str::to_string)).collect(),
            dirs: Vec::new(),
        }
    }

    fn battery(capacity: Option<u8>, status: Option<BatteryStatus>) -> SupplyReading {
        SupplyReading { kind: SupplyKind::Battery, capacity, status, online: None }
    }

    fn mains(online: Option<bool>) -> SupplyReading {
        SupplyReading { kind: SupplyKind::Mains, capacity: None, status: None, online }
    }

    #[test]
    fn parse_supply_reads_a_healthy_battery() {
        let s = parse_supply(Some("Battery\n"), Some("87\n"), Some("Discharging\n"), None);
        assert_eq!(s, battery(Some(87), Some(BatteryStatus::Discharging)));
    }

    #[test]
    fn parse_supply_reads_mains_online_flags() {
        assert_eq!(parse_supply(Some("Mains\n"), None, None, Some("1\n")), mains(Some(true)));
        assert_eq!(parse_supply(Some("Mains\n"), None, None, Some("0\n")), mains(Some(false)));
        assert_eq!(parse_supply(Some("Mains\n"), None, None, Some("what\n")), mains(None));
    }

    #[test]
    fn parse_supply_degrades_each_field_independently() {
        // Garbage capacity loses the capacity, nothing else.
        let s = parse_supply(Some("Battery"), Some("nonsense"), Some("Charging"), None);
        assert_eq!(s, battery(None, Some(BatteryStatus::Charging)));
        // Overrange capacity clamps rather than vanishing.
        assert_eq!(parse_supply(Some("Battery"), Some("120"), None, None).capacity, Some(100));
        // Unrecognized status maps to Unknown, not None — the file
        // existed and said something, which is different from absent.
        assert_eq!(parse_supply(Some("Battery"), None, Some("Levitating"), None).status, Some(BatteryStatus::Unknown));
        // Missing type keeps the supply as Other for summarize to judge.
        assert_eq!(parse_supply(None, Some("50"), None, None).kind, SupplyKind::Other);
    }

    #[test]
    fn summarize_maps_the_ordinary_laptop_states() {
        let chg = summarize(&[battery(Some(60), Some(BatteryStatus::Charging)), mains(Some(true))]);
        assert_eq!(chg, PowerFace::Battery { capacity: Some(60), state: ChargeState::Charging });
        let bat = summarize(&[battery(Some(80), Some(BatteryStatus::Discharging)), mains(Some(false))]);
        assert_eq!(bat, PowerFace::Battery { capacity: Some(80), state: ChargeState::Discharging });
        let full = summarize(&[battery(Some(100), Some(BatteryStatus::Full)), mains(Some(true))]);
        assert_eq!(full, PowerFace::Battery { capacity: Some(100), state: ChargeState::Full });
        let held = summarize(&[battery(Some(80), Some(BatteryStatus::NotCharging)), mains(Some(true))]);
        assert_eq!(held, PowerFace::Battery { capacity: Some(80), state: ChargeState::Full });
    }

    #[test]
    fn summarize_averages_multi_battery_packs_and_drain_wins_over_full() {
        let face = summarize(&[
            battery(Some(90), Some(BatteryStatus::Full)),
            battery(Some(30), Some(BatteryStatus::Discharging)),
        ]);
        assert_eq!(face, PowerFace::Battery { capacity: Some(60), state: ChargeState::Discharging });
    }

    #[test]
    fn summarize_falls_back_to_the_ac_line_when_statuses_are_silent() {
        let on_ac = summarize(&[battery(Some(50), Some(BatteryStatus::Unknown)), mains(Some(true))]);
        assert_eq!(on_ac, PowerFace::Battery { capacity: Some(50), state: ChargeState::Full });
        let off_ac = summarize(&[battery(Some(50), None), mains(Some(false))]);
        assert_eq!(off_ac, PowerFace::Battery { capacity: Some(50), state: ChargeState::Discharging });
    }

    #[test]
    fn summarize_classifies_a_typeless_battery_by_its_fields() {
        let face = summarize(&[parse_supply(None, Some("42"), Some("Discharging"), None)]);
        assert_eq!(face, PowerFace::Battery { capacity: Some(42), state: ChargeState::Discharging });
    }

    #[test]
    fn summarize_gives_desktops_the_ac_face_and_nothing_the_dead_face() {
        assert_eq!(summarize(&[mains(Some(true))]), PowerFace::AcOnly);
        assert_eq!(summarize(&[]), PowerFace::NoInfo);
        // A directory of supplies whose files were all unreadable says
        // nothing about power either.
        assert_eq!(summarize(&[parse_supply(None, None, None, None)]), PowerFace::NoInfo);
    }

    /// The end-to-end fold, on the laptop tree with a hole in it that
    /// `read_supplies_from` used to be tested against: two supplies,
    /// BAT0 missing its `status` file entirely, and the face that has
    /// to come out the other side. (The directory walk itself moved to
    /// a sampler thread and is tested there, against a real fixture
    /// directory, as `read_tree_walks_a_fixture_directory_and_degrades_per_field`.)
    #[test]
    fn update_folds_a_sampled_supply_tree_into_a_face() {
        let mut bench = SampleBench::new();
        let id = bench.tree(vec![
            supply("AC", [Some("Mains\n"), None, None, Some("1\n")]),
            supply("BAT0", [Some("Battery\n"), Some("73\n"), None, None]),
        ]);
        let mut widget = PowerWidget::new();
        widget.bind(&[id]);

        assert!(widget.update(&bench.samples()));
        assert_eq!(widget.face, PowerFace::Battery { capacity: Some(73), state: ChargeState::Full });

        // Unplugged: same tree, `online` flips, and the AC fallback
        // that was carrying `Full` stops carrying it.
        bench.set_tree(id, vec![
            supply("AC", [Some("Mains\n"), None, None, Some("0\n")]),
            supply("BAT0", [Some("Battery\n"), Some("71\n"), Some("Discharging\n"), None]),
        ]);
        assert!(widget.update(&bench.samples()));
        assert_eq!(widget.face, PowerFace::Battery { capacity: Some(71), state: ChargeState::Discharging });
    }

    /// Ten seconds between readings means most passes are stale, and a
    /// reading that says the same thing as the last one is not a
    /// repaint either — a battery gauge that redrew the dock every
    /// sample would repaint it 8,640 times a day to show the same
    /// number.
    #[test]
    fn only_a_changed_face_repaints() {
        let mut bench = SampleBench::new();
        let id = bench.tree(vec![supply("BAT0", [Some("Battery"), Some("50"), Some("Discharging"), None])]);
        let mut widget = PowerWidget::new();
        widget.bind(&[id]);
        assert!(widget.update(&bench.samples()));

        bench.all_stale();
        assert!(!widget.update(&bench.samples()), "a stale pass folds nothing");

        bench.set_tree(id, vec![supply("BAT0", [Some("Battery"), Some("50"), Some("Discharging"), None])]);
        assert!(!widget.update(&bench.samples()), "a fresh reading that says the same thing is not a change");
    }

    /// The dev-host case: no such directory, so the sampler delivers no
    /// entries and the tile shows the dead screen rather than inventing
    /// a battery.
    #[test]
    fn an_empty_tree_is_the_dead_face() {
        let mut bench = SampleBench::new();
        let id = bench.tree(Vec::new());
        let mut widget = PowerWidget::new();
        widget.bind(&[id]);
        assert!(!widget.update(&bench.samples()), "NoInfo was already the face");
        assert_eq!(widget.face, PowerFace::NoInfo);
    }
}
