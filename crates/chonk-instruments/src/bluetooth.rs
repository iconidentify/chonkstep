//! The Bluetooth instrument: is there a radio, is it on, and what is
//! connected to it — the data half of the [`wm_theme::bluetooth`]
//! instrument, and the host of the [`crate::bt_panel`] fold-out.
//!
//! This side reduces what the dock sampled on its behalf to the plain
//! values the pure renderer takes; the split (and the fixture-tested
//! parsers under `bt_panel`) is the pattern every tile in this crate
//! follows, and [`crate::WifiWidget`] is the closest sibling — the two
//! radios are meant to read as one family on the dock.
//!
//! # Three sources, and why none of them is `bluetoothctl`
//!
//! The obvious sampler for this tile is `bluetoothctl`, and it is
//! disqualified — not on parse-stability grounds but on the grounds
//! this whole crate exists to defend. On a machine with no adapter it
//! **hangs forever and prints nothing**: every subcommand measured on
//! the development host (`list`, `show`, `devices Connected`,
//! `devices Paired`) ran to a six-second kill with no output. The
//! dock's `BackgroundCommand` now runs every sampler command under a
//! deadline and kills it if it overruns (this instrument's report is
//! what put it there), so such a source would cost a dead face rather
//! than a wedged worker — but a source whose every run is killed at
//! the deadline is not a source, it is a stall with a timer on it.
//! `chonkstep never execs bluetoothctl`;
//! [`crate::bt_panel::bluez`]'s module doc carries the measurements and
//! the reasoning in full, including why Omarchy's own script wraps
//! every call it makes in `timeout 2s`.
//!
//! What it samples instead:
//!
//! * `/sys/class/bluetooth` as a [`Source::Tree`] — **does hardware
//!   exist**. A filesystem walk, so it cannot hang on an absent daemon,
//!   and it picks up a USB dongle on the sample after it is plugged in.
//!   This is deliberately a different question from the next one: a
//!   stopped `bluetooth.service` must not render as "you own no
//!   Bluetooth hardware".
//! * `busctl … GetManagedObjects` as a [`Source::Command`] — **what
//!   BlueZ says**: adapter power, and every known device with its name,
//!   connection state, icon class and battery, in one spawn per
//!   interval. On this machine it exits 1 in about no time, which the
//!   sampler already reads correctly as "no reading".
//! * `/sys/class/rfkill` as a [`Source::Tree`] — **which way a click
//!   should move**, the same role `nmcli radio wifi` plays for the link
//!   tile, and for the same reason: so `on_input` returns one
//!   [`Effect::Run`] instead of chaining a discovery round trip in
//!   front of a set. Slower than the others because nothing on the
//!   tile's face depends on it.
//!
//! # The four faces
//!
//! Click: toggle power, in `omarchy-bluetooth-power`'s order —
//! unblock if soft-blocked, block if on, ask BlueZ directly if
//! unblocked but dark. The toggle is a request, not a fact, so the
//! tile keeps showing reality until a sample confirms it.
//!
//! | State | Face |
//! |---|---|
//! | powered, devices connected | count digits + lit rune, first device's name |
//! | powered, nothing connected | ghost digits + lit rune, `READY` |
//! | adapter present, off or daemon silent | ghost rune, dim `OFF` |
//! | no adapter at all | the SDK's dead screen, `BT` |
//!
//! The last one is a first-class rendering, not a fallback: it is what
//! this instrument looks like on the machine it was written on, and on
//! every desktop without a controller. It is
//! [`wm_theme::panel::render_dead_tile`], exactly as the link tile
//! shows it for a machine with no NIC.

use std::path::PathBuf;
use std::time::Duration;

use chonk_dock_widget::{
    DockInput, DockWidget, Effect, PanelCtx, PanelEvent, PanelFrame, PanelReaction, PanelSpec, Samples, Source, SourceId,
    SAMPLE_INTERVAL,
};
use wm_theme::bluetooth::{render_bluetooth_tile, BtReading};
use wm_theme::{panel, Theme};
use wm_theme_api::DecorationBuffer;

use crate::bt_panel::bluez::{parse_managed_objects, rfkill_from, BluezState, RfkillState};
use crate::bt_panel::{set_powered_args, BtPanel};

/// Where the kernel publishes Bluetooth controllers. One directory per
/// controller (`hci0`, `hci1`); the *names* are the whole reading, so
/// no per-entry files are requested — presence is the question.
const BT_CLASS_ROOT: &str = "/sys/class/bluetooth";

/// The rfkill switches, and the three files that say what each one is
/// and whether it is blocking. Positional, like every
/// [`Source::Tree`]: this array and
/// [`rfkill_from`](crate::bt_panel::bluez::rfkill_from) stay in step.
const RFKILL_ROOT: &str = "/sys/class/rfkill";
const RFKILL_FIELDS: &[&str] = &["type", "soft", "hard"];

/// How often the rfkill block is re-read. Slower than
/// [`SAMPLE_INTERVAL`] for the same reason the link tile's radio source
/// is: nothing on the tile's face depends on it — it exists so a click
/// knows which direction to move — and three sysfs reads per switch is
/// not free either.
const RFKILL_INTERVAL: Duration = Duration::from_secs(2);

/// `busctl --system --json=short call org.bluez /org/bluez
/// org.freedesktop.DBus.ObjectManager GetManagedObjects`.
///
/// One call for the whole picture, rather than a `get-property` per
/// device: the object manager hands back every adapter and every known
/// device with all their interfaces in a single round trip, which is
/// the difference between one process spawn per interval and one per
/// device per interval. `--json=short` because the alternative is
/// `busctl`'s own nested-variant text format, and a machine-readable
/// surface with a documented shape beats a pretty-printer every time.
fn managed_objects_args() -> Vec<String> {
    ["--system", "--json=short", "call", "org.bluez", "/org/bluez", "org.freedesktop.DBus.ObjectManager", "GetManagedObjects"]
        .iter()
        .map(|arg| (*arg).to_string())
        .collect()
}

/// The tile's reading, owned-string mirror of `wm_theme::bluetooth`'s
/// borrowed [`BtReading`] plus `Absent`, which renders as the dead tile
/// rather than through the instrument.
#[derive(Clone, Debug, PartialEq, Eq)]
enum BtState {
    /// No controller in `/sys/class/bluetooth`. The dead screen.
    Absent,
    /// A controller exists but the radio is not on — powered down,
    /// rfkill-blocked, or `bluetoothd` not answering. Folded together
    /// deliberately: either way the radio is off, and a click's only
    /// honest offer is to try turning it on.
    Off,
    /// Powered, nothing connected.
    Idle,
    /// Powered, with `count` devices connected; `name` is the first
    /// one, for the lettering strip.
    Connected { count: u8, name: String },
}

pub struct BluetoothWidget {
    /// `/sys/class/bluetooth`, walked on a sampler thread.
    controllers: SourceId,
    /// `busctl … GetManagedObjects`, the BlueZ picture.
    bluez_src: SourceId,
    /// `/sys/class/rfkill`, for which way a click should move.
    rfkill_src: SourceId,
    state: BtState,
    present: bool,
    bluez: BluezState,
    rfkill: RfkillState,
    panel: BtPanel,
}

impl BluetoothWidget {
    pub fn new() -> Self {
        Self {
            controllers: SourceId::UNBOUND,
            bluez_src: SourceId::UNBOUND,
            rfkill_src: SourceId::UNBOUND,
            state: BtState::Absent,
            present: false,
            bluez: BluezState::default(),
            rfkill: RfkillState::default(),
            panel: BtPanel::new(),
        }
    }

    /// The face this reading deserves. Pure, and the whole of the
    /// tile's decision, so it can be tested without a renderer.
    fn derive(present: bool, bluez: &BluezState) -> BtState {
        if !present {
            return BtState::Absent;
        }
        if !bluez.any_powered() {
            return BtState::Off;
        }
        let connected: Vec<&str> = bluez.connected().map(|device| device.name.as_str()).collect();
        match connected.first() {
            Some(first) => BtState::Connected { count: connected.len().min(u8::MAX as usize) as u8, name: (*first).to_string() },
            None => BtState::Idle,
        }
    }

    /// The power click, shared by the tile and the panel's power row so
    /// the two cannot disagree about what a toggle means. See
    /// [`crate::bt_panel`]'s module doc for why it is three branches.
    fn power_effect(&self) -> Vec<Effect> {
        if self.rfkill.hard {
            // A physical kill switch: nothing runnable clears it, so
            // the honest answer is to do nothing rather than to fire a
            // command that will fail.
            return Vec::new();
        }
        if self.rfkill.soft_blocked() {
            return vec![Effect::Run {
                program: "rfkill",
                args: vec!["unblock".to_string(), "bluetooth".to_string()],
                then: Some(self.bluez_src),
            }];
        }
        if self.bluez.any_powered() {
            return vec![Effect::Run {
                program: "rfkill",
                args: vec!["block".to_string(), "bluetooth".to_string()],
                then: Some(self.bluez_src),
            }];
        }
        let Some(adapter) = self.bluez.primary() else {
            // Hardware exists but BlueZ is not answering, so there is no
            // adapter path to write `Powered` on. Unblocking is the one
            // move left that could bring the daemon's adapter up, and it
            // is harmless when nothing is blocked.
            return vec![Effect::Run {
                program: "rfkill",
                args: vec!["unblock".to_string(), "bluetooth".to_string()],
                then: Some(self.bluez_src),
            }];
        };
        vec![Effect::Run { program: "busctl", args: set_powered_args(&adapter.path, true), then: Some(self.bluez_src) }]
    }
}

impl Default for BluetoothWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl DockWidget for BluetoothWidget {
    fn name(&self) -> &str {
        "BT"
    }

    fn sources(&self) -> Vec<Source> {
        vec![
            Source::Tree { root: PathBuf::from(BT_CLASS_ROOT), files: &[], dirs: &[], interval: SAMPLE_INTERVAL },
            Source::Command { program: "busctl", args: managed_objects_args(), interval: SAMPLE_INTERVAL },
            Source::Tree { root: PathBuf::from(RFKILL_ROOT), files: RFKILL_FIELDS, dirs: &[], interval: RFKILL_INTERVAL },
        ]
    }

    fn bind(&mut self, ids: &[SourceId]) {
        self.controllers = ids.first().copied().unwrap_or(SourceId::UNBOUND);
        self.bluez_src = ids.get(1).copied().unwrap_or(SourceId::UNBOUND);
        self.rfkill_src = ids.get(2).copied().unwrap_or(SourceId::UNBOUND);
        self.panel.bind(self.bluez_src);
    }

    fn update(&mut self, samples: &Samples) -> bool {
        let bluez_fresh = samples.fresh(self.bluez_src);
        if !(samples.fresh(self.controllers) || bluez_fresh || samples.fresh(self.rfkill_src)) {
            return false;
        }
        let before = self.state.clone();

        // Every `hci*` directory is a controller. The entry names are
        // the entire reading — no files were requested, because
        // "does this exist" is the question.
        self.present = samples.tree(self.controllers).iter().any(|entry| entry.name.starts_with("hci"));
        self.rfkill = rfkill_from(samples.tree(self.rfkill_src));
        // A BlueZ reading that did not parse leaves the last good one
        // rather than blanking the panel mid-interaction; a *cleared*
        // slot (busctl exited non-zero, which is what an absent daemon
        // looks like) is an authoritative "nothing", and folds to the
        // off face through `any_powered`.
        self.bluez = match samples.text(self.bluez_src) {
            Some(output) => parse_managed_objects(output).unwrap_or_else(|| self.bluez.clone()),
            None if bluez_fresh => BluezState::default(),
            None => self.bluez.clone(),
        };

        self.state = Self::derive(self.present, &self.bluez);
        self.panel.set_state(self.present, &self.bluez, self.rfkill, bluez_fresh);
        self.state != before
    }

    fn render(&self, theme: &Theme, tile: u32, fonts: &mut cosmic_text::FontSystem, swash: &mut cosmic_text::SwashCache) -> DecorationBuffer {
        let reading = match &self.state {
            BtState::Absent => return panel::render_dead_tile(theme, fonts, swash, tile, "BT"),
            BtState::Off => BtReading::Off,
            BtState::Idle => BtReading::Idle,
            BtState::Connected { count, name } => BtReading::Connected { count: *count, name },
        };
        render_bluetooth_tile(theme, fonts, swash, tile, &reading)
    }

    fn on_input(&mut self, input: DockInput, _tile: u32) -> Vec<Effect> {
        if !matches!(input, DockInput::Press { .. }) {
            return Vec::new();
        }
        if !self.present {
            // No radio to turn on. A tile that fired a command here
            // would be pretending the machine has hardware it does not.
            return Vec::new();
        }
        // No `Repaint`: the toggle is a request and the face keeps
        // showing reality until a sample confirms it — the same
        // contract the link tile's radio toggle keeps.
        self.power_effect()
    }

    fn panel_spec(&self, tile: u32) -> Option<PanelSpec> {
        // The panel is offered even with no adapter: its `NO ADAPTER`
        // row is a real answer to "what Bluetooth does this machine
        // have", and a tile that simply ignored the gesture would leave
        // someone clicking at it wondering whether the dock was wedged.
        Some(self.panel.spec(tile))
    }

    fn render_panel(&mut self, frame: &mut PanelFrame, ctx: &mut PanelCtx<'_>) {
        let (width, height) = (frame.width(), frame.height());
        let buffer = self.panel.render(ctx.theme, ctx.tile, width, height, ctx.fonts, ctx.swash);
        frame.adopt(buffer);
    }

    fn panel_input(&mut self, event: PanelEvent, tile: u32) -> PanelReaction {
        self.panel.input(event, tile, wm_theme::bluetooth::panel_content_width(tile))
    }

    fn panel_tick(&mut self, now: std::time::Instant) -> bool {
        self.panel.tick(now)
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use chonk_dock_widget::{MouseButton, SampleBench, TreeEntry};
    use wm_theme_api::Point;

    fn press() -> DockInput {
        DockInput::Press { local: Point::new(28, 28), button: MouseButton::Left }
    }

    fn controller(name: &str) -> TreeEntry {
        TreeEntry { name: name.to_string(), files: Vec::new(), dirs: Vec::new() }
    }

    fn switch(kind: &str, soft: &str, hard: &str) -> TreeEntry {
        TreeEntry {
            name: "rfkill0".to_string(),
            files: vec![Some(format!("{kind}\n")), Some(format!("{soft}\n")), Some(format!("{hard}\n"))],
            dirs: Vec::new(),
        }
    }

    /// A canned `GetManagedObjects` reply: one powered adapter and the
    /// devices named, each `(name, connected, paired)`.
    fn bluez_reply(powered: bool, devices: &[(&str, bool, bool)]) -> String {
        let mut objects = vec![format!(
            r#""/org/bluez/hci0":{{"org.bluez.Adapter1":{{"Powered":{{"type":"b","data":{powered}}}}}}}"#
        )];
        for (index, (name, connected, paired)) in devices.iter().enumerate() {
            objects.push(format!(
                r#""/org/bluez/hci0/dev_00_00_00_00_00_{index:02X}":{{"org.bluez.Device1":{{"Alias":{{"type":"s","data":"{name}"}},"Connected":{{"type":"b","data":{connected}}},"Paired":{{"type":"b","data":{paired}}}}}}}"#
            ));
        }
        format!(r#"{{"type":"a{{oa{{sa{{sv}}}}}}","data":[{{{}}}]}}"#, objects.join(","))
    }

    /// The machine this instrument was written on: no controller, no
    /// rfkill switch, and a `busctl` that exits non-zero — which the
    /// sampler reports as a cleared slot. The dead face, and a click
    /// that does nothing at all.
    #[test]
    fn a_machine_with_no_adapter_is_absent_and_its_click_is_inert() {
        let mut bench = SampleBench::new();
        let controllers = bench.tree(Vec::new());
        let bluez = bench.missing();
        let rfkill = bench.tree(Vec::new());
        let mut widget = BluetoothWidget::new();
        widget.bind(&[controllers, bluez, rfkill]);

        widget.update(&bench.samples());
        assert_eq!(widget.state, BtState::Absent);
        assert!(widget.on_input(press(), 56).is_empty(), "there is no radio here to offer to turn on");
    }

    /// Hardware present but `bluetoothd` silent is *not* the same as no
    /// hardware: it is the off face, and its click is a real offer.
    #[test]
    fn a_present_adapter_with_a_silent_daemon_is_off_not_absent() {
        let mut bench = SampleBench::new();
        let controllers = bench.tree(vec![controller("hci0")]);
        let bluez = bench.missing();
        let rfkill = bench.tree(vec![switch("bluetooth", "0", "0")]);
        let mut widget = BluetoothWidget::new();
        widget.bind(&[controllers, bluez, rfkill]);

        widget.update(&bench.samples());
        assert_eq!(widget.state, BtState::Off);
        assert!(!widget.on_input(press(), 56).is_empty(), "a present adapter's click must still try");
    }

    #[test]
    fn update_folds_the_three_sources_into_one_face() {
        let mut bench = SampleBench::new();
        let controllers = bench.tree(vec![controller("hci0")]);
        let bluez = bench.text(&bluez_reply(true, &[]));
        let rfkill = bench.tree(vec![switch("bluetooth", "0", "0")]);
        let mut widget = BluetoothWidget::new();
        widget.bind(&[controllers, bluez, rfkill]);

        assert!(widget.update(&bench.samples()));
        assert_eq!(widget.state, BtState::Idle, "a powered radio with nothing on it is READY");

        bench.set_text(bluez, &bluez_reply(true, &[("WH-1000XM4", true, true), ("MX Keys", false, true)]));
        assert!(widget.update(&bench.samples()));
        assert_eq!(widget.state, BtState::Connected { count: 1, name: "WH-1000XM4".into() });

        bench.set_text(bluez, &bluez_reply(false, &[]));
        assert!(widget.update(&bench.samples()));
        assert_eq!(widget.state, BtState::Off);

        bench.all_stale();
        assert!(!widget.update(&bench.samples()), "a pass with nothing fresh folds nothing");
    }

    #[test]
    fn the_connected_count_is_the_connected_devices_not_the_known_ones() {
        let state = parse_managed_objects(&bluez_reply(
            true,
            &[("A", true, true), ("B", true, true), ("C", false, true), ("D", false, false)],
        ))
        .expect("parses");
        assert_eq!(BluetoothWidget::derive(true, &state), BtState::Connected { count: 2, name: "A".into() });
    }

    /// The power click, in `omarchy-bluetooth-power`'s order. A soft
    /// block is cleared *before* anything is asked of BlueZ, because a
    /// power-on fails outright while the block is set.
    #[test]
    fn a_soft_block_is_unblocked_rather_than_powered_through() {
        let mut bench = SampleBench::new();
        let controllers = bench.tree(vec![controller("hci0")]);
        let bluez = bench.text(&bluez_reply(false, &[]));
        let rfkill = bench.tree(vec![switch("bluetooth", "1", "0")]);
        let mut widget = BluetoothWidget::new();
        widget.bind(&[controllers, bluez, rfkill]);
        widget.update(&bench.samples());

        match &widget.on_input(press(), 56)[..] {
            [Effect::Run { program, args, then }] => {
                assert_eq!(*program, "rfkill", "never bluetoothctl, and never a power-on into a block");
                assert_eq!(args, &["unblock".to_string(), "bluetooth".to_string()]);
                assert_eq!(*then, Some(bluez), "the reading that decided the direction is what gets resampled");
            }
            other => panic!("expected one unblock, got {} effects", other.len()),
        }
    }

    /// Unblocked but dark: `AutoEnable` will not raise an adapter that
    /// was powered down without a block, so ask BlueZ directly.
    #[test]
    fn an_unblocked_dark_adapter_is_powered_through_busctl() {
        let mut bench = SampleBench::new();
        let controllers = bench.tree(vec![controller("hci0")]);
        let bluez = bench.text(&bluez_reply(false, &[]));
        let rfkill = bench.tree(vec![switch("bluetooth", "0", "0")]);
        let mut widget = BluetoothWidget::new();
        widget.bind(&[controllers, bluez, rfkill]);
        widget.update(&bench.samples());

        match &widget.on_input(press(), 56)[..] {
            [Effect::Run { program, args, .. }] => {
                assert_eq!(*program, "busctl");
                assert_eq!(args.last().map(String::as_str), Some("true"));
                assert!(args.contains(&"/org/bluez/hci0".to_string()), "the sampled adapter path is the runtime argument");
                assert!(args.contains(&"Powered".to_string()));
            }
            other => panic!("expected one busctl set-property, got {} effects", other.len()),
        }
    }

    /// Off is the rfkill block, not a `Powered` write: BlueZ never
    /// persists `Powered`, and the block is the half that survives a
    /// reboot.
    #[test]
    fn powering_off_sets_the_block_that_survives_a_reboot() {
        let mut bench = SampleBench::new();
        let controllers = bench.tree(vec![controller("hci0")]);
        let bluez = bench.text(&bluez_reply(true, &[]));
        let rfkill = bench.tree(vec![switch("bluetooth", "0", "0")]);
        let mut widget = BluetoothWidget::new();
        widget.bind(&[controllers, bluez, rfkill]);
        widget.update(&bench.samples());

        match &widget.on_input(press(), 56)[..] {
            [Effect::Run { program, args, .. }] => {
                assert_eq!(*program, "rfkill");
                assert_eq!(args, &["block".to_string(), "bluetooth".to_string()]);
            }
            other => panic!("expected one rfkill block, got {} effects", other.len()),
        }
    }

    /// A hardware kill switch is not ours to flip, so the click must
    /// not fire a command that is guaranteed to fail.
    #[test]
    fn a_hard_block_makes_the_click_inert() {
        let mut bench = SampleBench::new();
        let controllers = bench.tree(vec![controller("hci0")]);
        let bluez = bench.text(&bluez_reply(false, &[]));
        let rfkill = bench.tree(vec![switch("bluetooth", "1", "1")]);
        let mut widget = BluetoothWidget::new();
        widget.bind(&[controllers, bluez, rfkill]);
        widget.update(&bench.samples());
        assert!(widget.on_input(press(), 56).is_empty());
    }

    #[test]
    fn only_the_press_edge_acts() {
        let mut bench = SampleBench::new();
        let controllers = bench.tree(vec![controller("hci0")]);
        let bluez = bench.text(&bluez_reply(true, &[]));
        let rfkill = bench.tree(vec![switch("bluetooth", "0", "0")]);
        let mut widget = BluetoothWidget::new();
        widget.bind(&[controllers, bluez, rfkill]);
        widget.update(&bench.samples());

        let release = DockInput::Release { local: Point::new(28, 28), button: MouseButton::Left };
        assert!(widget.on_input(release, 56).is_empty());
    }

    /// A dongle appearing mid-session moves the tile off the dead face
    /// without a restart — the property the sysfs walk buys over
    /// asking BlueZ.
    #[test]
    fn a_hotplugged_controller_is_picked_up_on_the_next_sample() {
        let mut bench = SampleBench::new();
        let controllers = bench.tree(Vec::new());
        let bluez = bench.missing();
        let rfkill = bench.tree(Vec::new());
        let mut widget = BluetoothWidget::new();
        widget.bind(&[controllers, bluez, rfkill]);
        widget.update(&bench.samples());
        assert_eq!(widget.state, BtState::Absent);

        bench.set_tree(controllers, vec![controller("hci0")]);
        bench.set_text(bluez, &bluez_reply(true, &[("Buds", true, true)]));
        assert!(widget.update(&bench.samples()));
        assert_eq!(widget.state, BtState::Connected { count: 1, name: "Buds".into() });
    }

    /// The name every log line and tombstone tile will carry.
    #[test]
    fn the_widget_names_itself_the_way_the_dead_face_draws_it() {
        let widget = BluetoothWidget::new();
        assert_eq!(widget.name(), "BT");
        assert!(widget.name().len() <= 5 && widget.name() == widget.name().to_uppercase());
    }

    /// Rendering must not depend on a live system, at any size, in any
    /// state — including the absent one, which is the only one this
    /// machine can actually reach.
    #[test]
    fn every_face_renders_at_every_size() {
        let theme = wm_theme::default_theme::nextstep_classic();
        let mut fonts = cosmic_text::FontSystem::new();
        let mut swash = cosmic_text::SwashCache::new();
        let states =
            [BtState::Absent, BtState::Off, BtState::Idle, BtState::Connected { count: 3, name: "WH-1000XM4".into() }];
        for size in [16u32, 56, 112] {
            for state in &states {
                let mut widget = BluetoothWidget::new();
                widget.state = state.clone();
                let buffer = widget.render(&theme, size, &mut fonts, &mut swash);
                assert_eq!((buffer.width, buffer.height), (size, size), "{state:?} at {size}");
                assert_eq!(buffer.pixels.len(), (size * size * 4) as usize);
            }
        }
    }
}
