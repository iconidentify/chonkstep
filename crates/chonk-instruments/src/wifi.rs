//! Network link widget: which link the machine is on, how good it is,
//! and a radio toggle — the data half of the `wm_theme::wifi`
//! instrument. This side reduces what the dock sampled on its behalf to
//! the plain values the pure renderer takes; the split (and the
//! fixture-tested parsers below) is the pattern `widgets/mod.rs`
//! prescribes.
//!
//! The reading prefers NetworkManager when it exists: one
//! `nmcli -t -f ACTIVE,SSID,SIGNAL dev wifi` per sample gives the
//! associated SSID and signal. Whether or not nmcli answered with an
//! active wifi link, `/sys/class/net` is walked every sample anyway —
//! it is the only source of wired operstate/carrier/speed, and its
//! `wireless` subdirectory is the reliable wifi-hardware probe that
//! gates the click behavior. On a system with neither (the macOS dev
//! host), every probe comes back empty and the tile shows the SDK's
//! dead screen; a missing tool is never a panic and never a retry
//! storm — `nmcli`'s absence is remembered after the first failed
//! spawn.
//!
//! # Three sources, and why this widget most needed them
//!
//! This is the tile the whole rework came from, and it was still the
//! worst offender after the `nmcli` fix. `/sys/class/net` was walked on
//! the compositor's repaint thread once a second, four probes per
//! interface, and one of those probes is `speed`: reading it dispatches
//! into the driver's `ethtool` `get_link_ksettings` op, which on some
//! NICs blocks for hundreds of milliseconds and does so
//! uninterruptibly. There is no amount of care at this call site that
//! makes that safe on that thread. It is a [`Source::Tree`] now.
//!
//! The third source is new: `nmcli radio wifi`, sampled every two
//! seconds purely so a click knows which way to move the radio. Before
//! it, a click spawned a thread that ran `nmcli radio wifi` to discover
//! the state and then a second `nmcli radio wifi <off|on>` to set it —
//! two blocking round trips chained on a worker to answer a question
//! the dock could simply have been holding the answer to. Now
//! `on_input` returns one [`Effect::Run`].
//!
//! Click: with nmcli and wifi hardware, toggle the radio and let the
//! next sample pick up the result — the toggle is a request, not a
//! fact, so the tile keeps showing reality until the system confirms
//! it. Without that, clicking cycles which interface the tile watches,
//! pinning the choice so the auto-pick stops overriding it.

use std::path::PathBuf;
use std::time::Duration;

use wm_theme::wifi::{render_wifi_tile, LinkReading};
use wm_theme::{panel, Theme};
use wm_theme_api::DecorationBuffer;

use chonk_dock_widget::{DockInput, DockWidget, Effect, Samples, Source, SourceId, TreeEntry, SAMPLE_INTERVAL};

/// Where the interfaces live, and which of each interface's files the
/// tile reads. Positional, like every [`Source::Tree`]: the arrays and
/// [`probes_from`] have to stay in step.
const NET_ROOT: &str = "/sys/class/net";
const NET_FIELDS: &[&str] = &["operstate", "carrier", "speed"];
/// `wireless/` exists on a wifi interface whether the radio is on or
/// off, which is exactly the property the click gate needs — a radio
/// the user has turned off must still be a radio they can turn back on.
const NET_DIRS: &[&str] = &["wireless"];

/// How often the radio's on/off state is re-read. Slower than
/// `SAMPLE_INTERVAL` because nothing on the tile's face depends on it —
/// it exists only so a click knows which direction to move — and this
/// is a `nmcli` process spawn, which is not free.
const RADIO_INTERVAL: Duration = Duration::from_secs(2);

/// The nmcli query the sampler runs.
///
/// `list --rescan no` rather than a bare `dev wifi`, and that is the
/// difference between reading a cache and driving the radio. nmcli
/// documents the default as: *"nmcli ensures that the access point list
/// is no older than 30 seconds and triggers a network scan if
/// necessary."* A dock tile wants the currently associated network's
/// name and signal — information NetworkManager already has — not a
/// fresh survey of every access point in range. Asking for the survey
/// cost a ~3.6s blocking scan every ~34s, and kicking the radio into a
/// scan that often is its own small harm to an established connection.
///
/// Measured on the machine this was diagnosed on: 11-16ms with
/// `--rescan no`, against 3.5s without it.
fn nmcli_args() -> Vec<String> {
    ["-t", "-f", "ACTIVE,SSID,SIGNAL", "dev", "wifi", "list", "--rescan", "no"]
        .iter()
        .map(|arg| (*arg).to_string())
        .collect()
}

/// `nmcli radio wifi` — a cache read inside NetworkManager, no radio
/// involvement at all, which is why it is safe to run once every
/// [`RADIO_INTERVAL`] where `dev wifi` without `--rescan no` was not.
fn radio_args() -> Vec<String> {
    ["radio", "wifi"].iter().map(|arg| (*arg).to_string()).collect()
}

/// The argv that moves the radio the other way.
fn radio_set_args(enabled: bool) -> Vec<String> {
    ["radio", "wifi", if enabled { "off" } else { "on" }].iter().map(|arg| (*arg).to_string()).collect()
}

/// `nmcli radio wifi` prints exactly `enabled` or `disabled`. Anything
/// else — a future word, an error on stdout, an empty read — is `None`
/// rather than a guess, because guessing here means moving the user's
/// radio in the direction they did not ask for.
fn parse_nmcli_radio(output: &str) -> Option<bool> {
    match output.trim() {
        "enabled" => Some(true),
        "disabled" => Some(false),
        _ => None,
    }
}

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

/// One sampled `/sys/class/net` walk to the probes the widget decides
/// with, skipping loopback (there is no link quality to report on
/// `lo`). The sampler already sorted the entries by name and filtering
/// preserves that, so click-cycling has a stable order across samples.
fn probes_from(entries: &[TreeEntry]) -> Vec<IfaceProbe> {
    entries
        .iter()
        .filter(|entry| entry.name != "lo")
        .map(|entry| IfaceProbe {
            name: entry.name.clone(),
            up: entry.file(0).map(|s| s.trim() == "up").unwrap_or(false),
            carrier: entry.file(1).map(|s| s.trim() == "1").unwrap_or(false),
            speed_mbps: entry.file(2).and_then(parse_speed),
            wireless: entry.dir(0),
        })
        .collect()
}

pub struct WifiWidget {
    /// `/sys/class/net`, walked on a sampler thread.
    interfaces: SourceId,
    /// `nmcli dev wifi list --rescan no`, for the associated SSID and
    /// signal.
    wifi_list: SourceId,
    /// `nmcli radio wifi`, for which way a click should move it.
    radio: SourceId,
    state: LinkState,
    probes: Vec<IfaceProbe>,
    selected: usize,
    /// Whether `selected` was the user's click rather than the
    /// auto-pick — a pinned choice survives resampling until the
    /// interface itself disappears.
    pinned: bool,
    /// `None` until the first sample lands; `Some(false)` once the
    /// sampler reports nmcli unspawnable, which it never retries — so a
    /// system without NetworkManager pays one failed spawn total.
    nmcli: Option<bool>,
    /// The radio's last known state, `None` until `nmcli radio wifi`
    /// has answered with a word this code recognizes.
    radio_enabled: Option<bool>,
    wifi_hw: bool,
}

impl WifiWidget {
    pub fn new() -> Self {
        Self {
            interfaces: SourceId::UNBOUND,
            wifi_list: SourceId::UNBOUND,
            radio: SourceId::UNBOUND,
            state: LinkState::Absent,
            probes: Vec::new(),
            selected: 0,
            pinned: false,
            nmcli: None,
            radio_enabled: None,
            wifi_hw: false,
        }
    }

    /// The most recent nmcli reading, or `None` when nmcli is unusable,
    /// has not answered yet, or reports no active network. Also settles
    /// [`WifiWidget::nmcli`], since a sample that landed at all is the
    /// proof that nmcli runs here — which is what gates the
    /// click-to-toggle behavior.
    fn fold_nmcli(&mut self, samples: &Samples) -> Option<(String, u8)> {
        if samples.unusable(self.wifi_list) {
            self.nmcli = Some(false);
            return None;
        }
        let stdout = samples.text(self.wifi_list)?;
        self.nmcli = Some(true);
        parse_nmcli_wifi(stdout)
    }
}

impl Default for WifiWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl DockWidget for WifiWidget {
    fn name(&self) -> &str {
        "LNK"
    }

    fn sources(&self) -> Vec<Source> {
        vec![
            Source::Tree { root: PathBuf::from(NET_ROOT), files: NET_FIELDS, dirs: NET_DIRS, interval: SAMPLE_INTERVAL },
            Source::Command { program: "nmcli", args: nmcli_args(), interval: SAMPLE_INTERVAL },
            Source::Command { program: "nmcli", args: radio_args(), interval: RADIO_INTERVAL },
        ]
    }

    fn bind(&mut self, ids: &[SourceId]) {
        self.interfaces = ids.first().copied().unwrap_or(SourceId::UNBOUND);
        self.wifi_list = ids.get(1).copied().unwrap_or(SourceId::UNBOUND);
        self.radio = ids.get(2).copied().unwrap_or(SourceId::UNBOUND);
    }

    fn update(&mut self, samples: &Samples) -> bool {
        // The three sources run on different cadences and land on
        // different passes, and every one of them feeds the same
        // decision, so any of them being fresh re-derives all of it
        // from the readings currently held. Cheap: a handful of string
        // comparisons over at most a dozen interfaces.
        if !(samples.fresh(self.interfaces) || samples.fresh(self.wifi_list) || samples.fresh(self.radio)) {
            return false;
        }
        let before = self.state.clone();

        self.probes = probes_from(samples.tree(self.interfaces));
        if !self.pinned || self.selected >= self.probes.len() {
            self.selected = best_probe(&self.probes);
            self.pinned = false;
        }
        // `None` from the radio source is left as `None` rather than
        // overwriting a known state: a single unparseable read must not
        // make the next click refuse to fire.
        self.radio_enabled = samples.text(self.radio).and_then(parse_nmcli_radio).or(self.radio_enabled);

        let wifi = self.fold_nmcli(samples);
        // Hardware presence gates the click behavior, so it must not
        // depend on the radio being on: the sysfs `wireless` directory
        // exists either way, and an active association is proof enough
        // on systems where sysfs is unreadable.
        self.wifi_hw = self.probes.iter().any(|p| p.wireless) || wifi.is_some();
        self.state = match wifi {
            Some((ssid, signal)) => LinkState::Wifi { ssid, signal },
            None => pick_link(&self.probes, self.selected),
        };
        self.state != before
    }

    fn render(&self, theme: &Theme, tile: u32, fonts: &mut cosmic_text::FontSystem, swash: &mut cosmic_text::SwashCache) -> DecorationBuffer {
        let reading = match &self.state {
            LinkState::Absent => return panel::render_dead_tile(theme, fonts, swash, tile, "LNK"),
            LinkState::Wifi { ssid, signal } => LinkReading::Wifi { ssid, signal_pct: *signal },
            LinkState::Wired { interface, speed_mbps } => LinkReading::Wired { interface, speed_mbps: *speed_mbps },
            LinkState::Down { interface } => LinkReading::Down { interface },
        };
        render_wifi_tile(theme, fonts, swash, tile, &reading)
    }

    fn on_input(&mut self, input: DockInput, _tile: u32) -> Vec<Effect> {
        if !matches!(input, DockInput::Press { .. }) {
            return Vec::new();
        }
        if self.nmcli == Some(true) && self.wifi_hw {
            // Set the opposite of the *actual* state rather than
            // blind-toggling, so a click always moves the radio away
            // from where it is even if something else moved it first.
            // The state is already here because it is a declared
            // source; it used to cost a `nmcli radio wifi` round trip
            // chained in front of the set, on a thread, per click.
            let Some(enabled) = self.radio_enabled else {
                // nmcli runs, but its radio state has not parsed. Ask
                // for it now rather than moving the radio on a guess —
                // the wrong guess turns the user's wifi off.
                return vec![Effect::Resample(self.radio)];
            };
            // No `Repaint`: the toggle is a request and the face keeps
            // showing reality until a sample confirms it. `then` points
            // at the radio source rather than the link list because
            // that is the reading the *next* click depends on; the link
            // list catches up on its own one-second cadence, which is
            // the same second the tile already lives with.
            vec![Effect::Run { program: "nmcli", args: radio_set_args(enabled), then: Some(self.radio) }]
        } else if self.probes.len() > 1 {
            self.selected = (self.selected + 1) % self.probes.len();
            self.pinned = true;
            let before = std::mem::replace(&mut self.state, pick_link(&self.probes, self.selected));
            if self.state == before {
                Vec::new()
            } else {
                vec![Effect::Repaint]
            }
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chonk_dock_widget::SampleBench;
    use chonk_dock_widget::MouseButton;
    use wm_theme_api::Point;

    fn press() -> DockInput {
        DockInput::Press { local: Point::new(28, 28), button: MouseButton::Left }
    }

    /// One `/sys/class/net/<name>` directory as the sampler would have
    /// delivered it, positional against `NET_FIELDS` and `NET_DIRS`.
    fn iface(name: &str, operstate: &str, carrier: &str, speed: Option<&str>, wireless: bool) -> TreeEntry {
        TreeEntry {
            name: name.to_string(),
            files: vec![Some(operstate.to_string()), Some(carrier.to_string()), speed.map(str::to_string)],
            dirs: vec![wireless],
        }
    }

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

    #[test]
    fn nmcli_radio_reads_only_the_two_words_it_knows() {
        assert_eq!(parse_nmcli_radio("enabled\n"), Some(true));
        assert_eq!(parse_nmcli_radio("disabled"), Some(false));
        for unknown in ["", "on", "off", "missing", "Error: NetworkManager is not running."] {
            assert_eq!(parse_nmcli_radio(unknown), None, "{unknown:?} must not become a direction to move the radio");
        }
    }

    #[test]
    fn probes_from_a_sampled_tree_drops_loopback_and_keeps_the_walk_order() {
        let entries = vec![
            iface("enp0s1", "up\n", "1\n", Some("1000\n"), false),
            iface("lo", "unknown\n", "1\n", None, false),
            iface("wlan0", "down\n", "0\n", Some("-1\n"), true),
        ];
        let probes = probes_from(&entries);
        assert_eq!(probes.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), vec!["enp0s1", "wlan0"]);
        assert_eq!(probes[0], IfaceProbe { name: "enp0s1".into(), up: true, carrier: true, speed_mbps: Some(1000), wireless: false });
        assert_eq!(probes[1].speed_mbps, None, "-1 is the kernel's 'unknown', not a speed");
        assert!(probes[1].wireless, "the wireless/ subdirectory is what gates the radio toggle");
    }

    /// The wired path end to end: a sysfs tree in, a link state out,
    /// with nmcli present but reporting no association.
    #[test]
    fn update_folds_the_three_sources_into_one_link_state() {
        let mut bench = SampleBench::new();
        let net = bench.tree(vec![iface("enp0s1", "up\n", "1\n", Some("1000\n"), false)]);
        let list = bench.text("no:Neighbors:52\n");
        let radio = bench.text("enabled\n");
        let mut widget = WifiWidget::new();
        widget.bind(&[net, list, radio]);

        assert!(widget.update(&bench.samples()));
        assert_eq!(widget.state, LinkState::Wired { interface: "enp0s1".into(), speed_mbps: Some(1000) });
        assert_eq!(widget.nmcli, Some(true));
        assert_eq!(widget.radio_enabled, Some(true));
        assert!(!widget.wifi_hw, "no wireless/ and no association means no radio to toggle");

        // Associating to a network beats the wired probe: the SSID is
        // the more specific answer about what the machine is on.
        bench.set_text(list, "yes:HomeBase:87\n");
        assert!(widget.update(&bench.samples()));
        assert_eq!(widget.state, LinkState::Wifi { ssid: "HomeBase".into(), signal: 87 });
        assert!(widget.wifi_hw);

        bench.all_stale();
        assert!(!widget.update(&bench.samples()), "a pass with nothing fresh folds nothing");
    }

    /// The click improvement, stated as a test: with the radio state
    /// already sampled, one press is one effect — no discovery round
    /// trip, and no repaint, because the toggle is a request.
    #[test]
    fn a_press_with_wifi_hardware_emits_exactly_one_run_effect() {
        let mut bench = SampleBench::new();
        let net = bench.tree(vec![iface("wlan0", "up\n", "1\n", None, true)]);
        let list = bench.text("yes:HomeBase:87\n");
        let radio = bench.text("enabled\n");
        let mut widget = WifiWidget::new();
        widget.bind(&[net, list, radio]);
        widget.update(&bench.samples());

        let effects = widget.on_input(press(), 56);
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::Run { program, args, then } => {
                assert_eq!(*program, "nmcli");
                assert_eq!(args, &vec!["radio".to_string(), "wifi".to_string(), "off".to_string()], "an enabled radio is turned off");
                assert_eq!(*then, Some(radio), "and the state that decided the direction is what gets resampled");
            }
            _ => panic!("a press on a wifi tile must be a Run effect"),
        }

        // The other direction, from the other sampled state.
        bench.set_text(radio, "disabled\n");
        widget.update(&bench.samples());
        match &widget.on_input(press(), 56)[0] {
            Effect::Run { args, .. } => assert_eq!(args.last().map(String::as_str), Some("on")),
            _ => panic!("a press on a wifi tile must be a Run effect"),
        }
    }

    /// nmcli runs but has not said anything this code understands: ask
    /// again rather than moving the radio on a guess. The wrong guess
    /// turns the user's wifi off for them.
    #[test]
    fn a_press_with_an_unknown_radio_state_asks_rather_than_guesses() {
        let mut bench = SampleBench::new();
        let net = bench.tree(vec![iface("wlan0", "up\n", "1\n", None, true)]);
        let list = bench.text("yes:HomeBase:87\n");
        let radio = bench.text("Error: NetworkManager is not running.\n");
        let mut widget = WifiWidget::new();
        widget.bind(&[net, list, radio]);
        widget.update(&bench.samples());

        assert!(widget.radio_enabled.is_none());
        assert!(matches!(widget.on_input(press(), 56)[..], [Effect::Resample(id)] if id == radio));
    }

    /// No nmcli at all — the dev host, or a machine on systemd-networkd
    /// — so a press cycles which interface the tile watches and pins
    /// the choice against the auto-pick.
    #[test]
    fn without_nmcli_a_press_cycles_and_pins_the_watched_interface() {
        let mut bench = SampleBench::new();
        let net = bench.tree(vec![
            iface("enp0s1", "up\n", "1\n", Some("1000\n"), false),
            iface("enp0s2", "down\n", "0\n", None, false),
        ]);
        let list = bench.unusable();
        let radio = bench.unusable();
        let mut widget = WifiWidget::new();
        widget.bind(&[net, list, radio]);
        widget.update(&bench.samples());

        assert_eq!(widget.nmcli, Some(false));
        assert_eq!(widget.selected, 0, "the live interface is the auto-pick");
        assert!(matches!(widget.on_input(press(), 56)[..], [Effect::Repaint]));
        assert_eq!(widget.selected, 1);
        assert!(widget.pinned);
        assert_eq!(widget.state, LinkState::Down { interface: "enp0s2".into() });

        // A pin survives resampling: the auto-pick must not drag the
        // tile back to the live interface the moment the user looked
        // away from it.
        bench.set_tree(net, vec![
            iface("enp0s1", "up\n", "1\n", Some("1000\n"), false),
            iface("enp0s2", "down\n", "0\n", None, false),
        ]);
        widget.update(&bench.samples());
        assert_eq!(widget.selected, 1);
    }

    /// A release is the same click's other edge; acting on both would
    /// cycle twice or toggle the radio twice per click.
    #[test]
    fn only_the_press_edge_acts() {
        let mut bench = SampleBench::new();
        let net = bench.tree(vec![iface("wlan0", "up\n", "1\n", None, true)]);
        let list = bench.text("yes:HomeBase:87\n");
        let radio = bench.text("enabled\n");
        let mut widget = WifiWidget::new();
        widget.bind(&[net, list, radio]);
        widget.update(&bench.samples());

        let release = DockInput::Release { local: Point::new(28, 28), button: MouseButton::Left };
        assert!(widget.on_input(release, 56).is_empty());
    }
}
