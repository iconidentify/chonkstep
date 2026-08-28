//! Power instrument: battery capacity, charge state, AC presence,
//! sampled from `/sys/class/power_supply` and rendered by
//! `wm_theme::power` — this file is the data half of the split the
//! widget SDK prescribes, so everything here is sampling and
//! summarizing, no drawing.
//!
//! The sampling degrades per-field on purpose: a supply directory with
//! an unreadable `capacity` still contributes its `status`, one with a
//! missing `type` is still classified by the battery-shaped fields it
//! does have, and only a machine exposing *nothing* falls through to
//! the renderer's dead screen. Desktops and VMs — a `Mains` supply and
//! no battery — get the deliberate "on line power" face instead.

use std::cell::RefCell;
use std::path::Path;
use std::time::{Duration, Instant};

use wm_theme::power::{render_power_tile, ChargeState, PowerFace};
use wm_theme::Theme;
use wm_theme_api::DecorationBuffer;

use super::DockWidget;

/// Sampling throttle — deliberately wider than the shared
/// `SAMPLE_INTERVAL`: battery percentage moves on the scale of minutes,
/// and every sample is a handful of sysfs reads that wake the disk of
/// exactly nobody but still cost syscalls per tick.
const POWER_SAMPLE_INTERVAL: Duration = Duration::from_secs(10);

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
    } else if any(BatteryStatus::Full) || any(BatteryStatus::NotCharging) {
        ChargeState::Full
    } else if ac_online {
        ChargeState::Full
    } else {
        ChargeState::Discharging
    };
    PowerFace::Battery { capacity, state }
}

/// The thin IO shell around [`parse_supply`]: one directory per supply,
/// one small file per field, absent files read as `None`. Parameterized
/// on the root so tests can point it at a fixture tree.
fn read_supplies_from(root: &Path) -> Vec<SupplyReading> {
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        let field = |name: &str| std::fs::read_to_string(dir.join(name)).ok();
        let (kind, capacity, status, online) = (field("type"), field("capacity"), field("status"), field("online"));
        out.push(parse_supply(kind.as_deref(), capacity.as_deref(), status.as_deref(), online.as_deref()));
    }
    out
}

fn read_supplies() -> Vec<SupplyReading> {
    read_supplies_from(Path::new("/sys/class/power_supply"))
}

pub struct PowerWidget {
    last_sample: Instant,
    face: PowerFace,
    font_system: RefCell<cosmic_text::FontSystem>,
    swash_cache: RefCell<cosmic_text::SwashCache>,
}

impl PowerWidget {
    pub fn new() -> Self {
        Self {
            // Backdated so the first tick samples immediately instead
            // of showing the dead screen for the first ten seconds.
            last_sample: Instant::now() - POWER_SAMPLE_INTERVAL,
            face: PowerFace::NoInfo,
            font_system: RefCell::new(cosmic_text::FontSystem::new()),
            swash_cache: RefCell::new(cosmic_text::SwashCache::new()),
        }
    }
}

impl Default for PowerWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl DockWidget for PowerWidget {
    fn tick(&mut self) -> bool {
        if self.last_sample.elapsed() < POWER_SAMPLE_INTERVAL {
            return false;
        }
        self.last_sample = Instant::now();
        let face = summarize(&read_supplies());
        if face == self.face {
            return false;
        }
        self.face = face;
        true
    }

    fn render(&self, theme: &Theme, tile: u32) -> DecorationBuffer {
        let mut font_system = self.font_system.borrow_mut();
        let mut swash_cache = self.swash_cache.borrow_mut();
        render_power_tile(theme, &mut font_system, &mut swash_cache, tile, self.face)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn read_supplies_from_walks_a_fixture_tree_with_holes() {
        let root = std::env::temp_dir().join(format!("chonkstep-power-fixture-{}", std::process::id()));
        let bat = root.join("BAT0");
        let ac = root.join("AC");
        std::fs::create_dir_all(&bat).unwrap();
        std::fs::create_dir_all(&ac).unwrap();
        std::fs::write(bat.join("type"), "Battery\n").unwrap();
        std::fs::write(bat.join("capacity"), "73\n").unwrap();
        // No status file at all: the field must degrade, not the supply.
        std::fs::write(ac.join("type"), "Mains\n").unwrap();
        std::fs::write(ac.join("online"), "1\n").unwrap();

        let mut supplies = read_supplies_from(&root);
        supplies.sort_by_key(|s| s.kind == SupplyKind::Mains);
        assert_eq!(
            supplies,
            vec![
                SupplyReading { kind: SupplyKind::Battery, capacity: Some(73), status: None, online: None },
                mains(Some(true)),
            ]
        );
        assert_eq!(summarize(&supplies), PowerFace::Battery { capacity: Some(73), state: ChargeState::Full });

        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(read_supplies_from(&root), Vec::new(), "a missing sysfs root reads as no supplies");
    }
}
