//! Network link widget: which link the machine is on, how good it is,
//! and a radio toggle — the data half of the `wm_theme::wifi`
//! instrument. This side samples the system and reduces it to the
//! plain values the pure renderer takes; the split (and the
//! fixture-tested parsers below) is the pattern `widgets/mod.rs`
//! prescribes.
//!
//! Sampling prefers NetworkManager when it exists: one
//! `nmcli -t -f ACTIVE,SSID,SIGNAL dev wifi` per sample gives the
//! associated SSID and signal. Whether or not nmcli answered with an
//! active wifi link, `/sys/class/net` is scanned every sample anyway —
//! it is a handful of file reads, it is the only source of wired
//! operstate/carrier/speed, and its `wireless` subdirectory is the
//! reliable wifi-hardware probe that gates the click behavior. On a
//! system with neither (the macOS dev host), every probe fails
//! quietly and the tile shows the SDK's dead screen; a missing tool is
//! never a panic and never a retry storm — `nmcli`'s absence is
//! remembered after the first failed spawn.
//!
//! Click: with nmcli and wifi hardware, toggle the radio (query
//! `nmcli radio wifi`, then set the opposite) and let the next sample
//! pick up the result — the toggle is a request, not a fact, so the
//! tile keeps showing reality until the system confirms it. Without
//! that, clicking cycles which interface the tile watches, pinning the
//! choice so the auto-pick stops overriding it.

use std::cell::RefCell;
use std::process::Command;
use std::time::Instant;

use wm_theme::wifi::{render_wifi_tile, LinkReading};
use wm_theme::{panel, Theme};
use wm_theme_api::{DecorationBuffer, Point};

use super::{DockWidget, SAMPLE_INTERVAL};

/// The sampled link, owned-string mirror of `wm_theme::wifi`'s
/// borrowed `LinkReading` (plus `Absent`, which renders as the dead
/// tile rather than through the instrument).
#[derive(Clone, Debug, PartialEq, Eq)]
enum LinkState {
    Absent,
    Down { interface: String },
    Wifi { ssid: String, signal: u8 },
    Wired { interface: String, speed_mbps: Option<u32> },
}

/// One `/sys/class/net` interface, reduced to the fields the widget
/// decides with. Kept separate from `LinkState` so the decision
/// (`pick_link`) is a pure function over a slice of these.
#[derive(Clone, Debug, PartialEq, Eq)]
struct IfaceProbe {
    name: String,
    up: bool,
    carrier: bool,
    speed_mbps: Option<u32>,
    wireless: bool,
}

/// Splits one nmcli terse-mode line on unescaped `:`. Terse mode
/// backslash-escapes both `:` and `\` inside values (an SSID may
/// legally contain either), so a naive `split(':')` would shear such
/// an SSID apart and misread its tail as the signal.
fn split_terse(line: &str) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(escaped) = chars.next() {
                    fields.last_mut().expect("fields never empty").push(escaped);
                }
            }
            ':' => fields.push(String::new()),
            _ => fields.last_mut().expect("fields never empty").push(c),
        }
    }
    fields
}

/// The active line of `nmcli -t -f ACTIVE,SSID,SIGNAL dev wifi`:
/// `yes:MyNet:87` means associated to MyNet at signal 87. Signal is
/// the last field and ACTIVE the first; everything between is the
/// SSID (rejoined on the off chance a raw colon survives). Lines that
/// do not parse are skipped, not fatal — nmcli's output is input, not
/// a contract.
fn parse_nmcli_wifi(output: &str) -> Option<(String, u8)> {
    for line in output.lines() {
        let fields = split_terse(line);
        if fields.len() < 3 || fields[0] != "yes" {
            continue;
        }
        let Ok(signal) = fields[fields.len() - 1].trim().parse::<u8>() else { continue };
        let ssid = fields[1..fields.len() - 1].join(":");
        return Some((ssid, signal.min(100)));
    }
    None
}

/// `/sys/class/net/*/speed` contents to Mb/s. The file may be absent
/// (no read at all), unreadable (drivers return EINVAL while the link
/// is down), report `-1` (link up, speed unknown), or hold a real
/// value; only a positive number is a speed.
fn parse_speed(raw: &str) -> Option<u32> {
    let value: i64 = raw.trim().parse().ok()?;
    u32::try_from(value).ok().filter(|v| *v > 0)
}

/// The first interface worth watching unpinned: the first live one,
/// else the first at all — a machine with one dead port should show
/// that port's down face, not `Absent`.
fn best_probe(probes: &[IfaceProbe]) -> usize {
    probes.iter().position(|p| p.up && p.carrier).unwrap_or(0)
}

/// The watched interface's state. `up` and `carrier` both gate the
/// live face: an administratively-up port with no cable is exactly
/// what the down face exists to show.
fn pick_link(probes: &[IfaceProbe], selected: usize) -> LinkState {
    let Some(probe) = probes.get(selected).or_else(|| probes.first()) else {
        return LinkState::Absent;
    };
    if probe.up && probe.carrier {
        LinkState::Wired { interface: probe.name.clone(), speed_mbps: probe.speed_mbps }
    } else {
        LinkState::Down { interface: probe.name.clone() }
    }
}

/// Scans `/sys/class/net`, skipping loopback (there is no link quality
/// to report on `lo`). Sorted by name so click-cycling has a stable
/// order across samples.
fn probe_sysfs() -> Vec<IfaceProbe> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/sys/class/net") else { return out };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "lo" {
            continue;
        }
        let dir = entry.path();
        let read = |file: &str| std::fs::read_to_string(dir.join(file));
        out.push(IfaceProbe {
            up: read("operstate").map(|s| s.trim() == "up").unwrap_or(false),
            carrier: read("carrier").map(|s| s.trim() == "1").unwrap_or(false),
            speed_mbps: read("speed").ok().and_then(|s| parse_speed(&s)),
            wireless: dir.join("wireless").exists(),
            name,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub struct WifiWidget {
    last_sample: Instant,
    state: LinkState,
    probes: Vec<IfaceProbe>,
    selected: usize,
    /// Whether `selected` was the user's click rather than the
    /// auto-pick — a pinned choice survives resampling until the
    /// interface itself disappears.
    pinned: bool,
    /// `None` until the first spawn attempt; `Some(false)` after one
    /// failure and never retried, so a system without NetworkManager
    /// pays one failed spawn total, not one per second.
    nmcli: Option<bool>,
    wifi_hw: bool,
    font_system: RefCell<cosmic_text::FontSystem>,
    swash_cache: RefCell<cosmic_text::SwashCache>,
}

impl WifiWidget {
    pub fn new() -> Self {
        Self {
            last_sample: Instant::now() - SAMPLE_INTERVAL,
            state: LinkState::Absent,
            probes: Vec::new(),
            selected: 0,
            pinned: false,
            nmcli: None,
            wifi_hw: false,
            font_system: RefCell::new(cosmic_text::FontSystem::new()),
            swash_cache: RefCell::new(cosmic_text::SwashCache::new()),
        }
    }

    /// One nmcli query, or `None` when nmcli is (now known to be)
    /// unusable. Any spawn error disables further attempts: NotFound
    /// means no NetworkManager, and anything else is equally beyond a
    /// dock widget's power to fix by retrying every second.
    fn nmcli_wifi(&mut self) -> Option<(String, u8)> {
        if self.nmcli == Some(false) {
            return None;
        }
        match Command::new("nmcli").args(["-t", "-f", "ACTIVE,SSID,SIGNAL", "dev", "wifi"]).output() {
            Ok(output) => {
                self.nmcli = Some(true);
                if !output.status.success() {
                    return None;
                }
                parse_nmcli_wifi(&String::from_utf8_lossy(&output.stdout))
            }
            Err(_) => {
                self.nmcli = Some(false);
                None
            }
        }
    }

    fn sample_now(&mut self) {
        self.probes = probe_sysfs();
        if !self.pinned || self.selected >= self.probes.len() {
            self.selected = best_probe(&self.probes);
            self.pinned = false;
        }
        let wifi = self.nmcli_wifi();
        // Hardware presence gates the click behavior, so it must not
        // depend on the radio being on: the sysfs `wireless` directory
        // exists either way, and an active association is proof enough
        // on systems where sysfs is unreadable.
        self.wifi_hw = self.probes.iter().any(|p| p.wireless) || wifi.is_some();
        self.state = match wifi {
            Some((ssid, signal)) => LinkState::Wifi { ssid, signal },
            None => pick_link(&self.probes, self.selected),
        };
    }
}

impl Default for WifiWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl DockWidget for WifiWidget {
    fn tick(&mut self) -> bool {
        if self.last_sample.elapsed() < SAMPLE_INTERVAL {
            return false;
        }
        self.last_sample = Instant::now();
        let before = self.state.clone();
        self.sample_now();
        self.state != before
    }

    fn render(&self, theme: &Theme, tile: u32) -> DecorationBuffer {
        let mut font_system = self.font_system.borrow_mut();
        let mut swash_cache = self.swash_cache.borrow_mut();
        let reading = match &self.state {
            LinkState::Absent => return panel::render_dead_tile(theme, &mut font_system, &mut swash_cache, tile, "LNK"),
            LinkState::Wifi { ssid, signal } => LinkReading::Wifi { ssid, signal_pct: *signal },
            LinkState::Wired { interface, speed_mbps } => LinkReading::Wired { interface, speed_mbps: *speed_mbps },
            LinkState::Down { interface } => LinkReading::Down { interface },
        };
        render_wifi_tile(theme, &mut font_system, &mut swash_cache, tile, &reading)
    }

    fn on_click(&mut self, _local: Point, _tile: u32) -> bool {
        if self.nmcli == Some(true) && self.wifi_hw {
            // Query-then-set instead of a blind toggle, so a click
            // always moves the radio away from its *actual* state. The
            // pixels do not change here: the toggle is a request, and
            // the face keeps showing reality until the (immediately
            // rescheduled) next sample confirms it.
            let enabled = Command::new("nmcli")
                .args(["radio", "wifi"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "enabled");
            if let Some(enabled) = enabled {
                let _ = Command::new("nmcli").args(["radio", "wifi", if enabled { "off" } else { "on" }]).output();
                self.last_sample = Instant::now() - SAMPLE_INTERVAL;
            }
            false
        } else if self.probes.len() > 1 {
            self.selected = (self.selected + 1) % self.probes.len();
            self.pinned = true;
            let before = std::mem::replace(&mut self.state, pick_link(&self.probes, self.selected));
            self.state != before
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nmcli_active_line_parses_ssid_and_signal() {
        let out = "no:Neighbors:52\nyes:HomeBase:87\nno:Cafe:31\n";
        assert_eq!(parse_nmcli_wifi(out), Some(("HomeBase".to_string(), 87)));
    }

    #[test]
    fn nmcli_escaped_colons_and_backslashes_stay_in_the_ssid() {
        assert_eq!(parse_nmcli_wifi("yes:Lab\\:5G:64\n"), Some(("Lab:5G".to_string(), 64)));
        assert_eq!(parse_nmcli_wifi("yes:Back\\\\slash:12\n"), Some(("Back\\slash".to_string(), 12)));
    }

    #[test]
    fn nmcli_without_an_active_network_yields_none() {
        assert_eq!(parse_nmcli_wifi("no:One:40\nno:Two:80\n"), None);
        assert_eq!(parse_nmcli_wifi(""), None);
    }

    #[test]
    fn nmcli_garbage_lines_are_skipped_not_fatal() {
        assert_eq!(parse_nmcli_wifi("yes:Broken:notanumber\nyes:Good:55\n"), Some(("Good".to_string(), 55)));
        assert_eq!(parse_nmcli_wifi("yes:TooHot:250\n"), Some(("TooHot".to_string(), 100)), "signal clamps to 100");
    }

    #[test]
    fn hidden_ssid_still_parses() {
        assert_eq!(parse_nmcli_wifi("yes::71\n"), Some((String::new(), 71)));
    }

    #[test]
    fn sysfs_speed_handles_the_unusable_values() {
        assert_eq!(parse_speed("1000\n"), Some(1000));
        assert_eq!(parse_speed("2500"), Some(2500));
        assert_eq!(parse_speed("-1\n"), None, "-1 is the kernel's 'unknown'");
        assert_eq!(parse_speed("0"), None);
        assert_eq!(parse_speed(""), None);
        assert_eq!(parse_speed("fast"), None);
    }

    fn probe(name: &str, up: bool, carrier: bool, speed_mbps: Option<u32>) -> IfaceProbe {
        IfaceProbe { name: name.to_string(), up, carrier, speed_mbps, wireless: false }
    }

    #[test]
    fn no_interfaces_means_absent() {
        assert_eq!(pick_link(&[], 0), LinkState::Absent);
    }

    #[test]
    fn a_live_wired_interface_reports_its_speed() {
        let probes = [probe("enp0s1", true, true, Some(1000))];
        assert_eq!(pick_link(&probes, 0), LinkState::Wired { interface: "enp0s1".to_string(), speed_mbps: Some(1000) });
    }

    #[test]
    fn up_without_carrier_is_down_not_live() {
        let probes = [probe("eth0", true, false, None)];
        assert_eq!(pick_link(&probes, 0), LinkState::Down { interface: "eth0".to_string() });
    }

    #[test]
    fn best_probe_prefers_the_live_interface_but_settles_for_a_dead_one() {
        let probes = [probe("eth0", false, false, None), probe("eth1", true, true, Some(100))];
        assert_eq!(best_probe(&probes), 1);
        let all_dead = [probe("eth0", false, false, None), probe("eth1", false, false, None)];
        assert_eq!(best_probe(&all_dead), 0);
    }

    #[test]
    fn out_of_range_selection_falls_back_to_the_first_interface() {
        let probes = [probe("eth0", true, true, Some(100))];
        assert_eq!(pick_link(&probes, 5), LinkState::Wired { interface: "eth0".to_string(), speed_mbps: Some(100) });
    }
}
