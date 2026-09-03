//! BlueZ, read through `busctl`: the adapter and the known devices,
//! reduced from one `GetManagedObjects` reply to the plain values the
//! tile and the panel decide with.
//!
//! # Why `busctl` and not `bluetoothctl`
//!
//! This is the load-bearing decision in the whole instrument, it was
//! made against measurement rather than taste, and it is the opposite
//! of what the obvious sampler would be — so it is written down here
//! rather than left to be rediscovered.
//!
//! `bluetoothctl` is the tool everyone reaches for. On a machine with
//! no adapter it **hangs forever**. Measured on the development host
//! (no controller, `/sys/class/bluetooth` absent, `bluetooth.service`
//! inactive), every one of these ran to a six-second `timeout(1)` kill
//! and produced no output at all:
//!
//! ```text
//! bluetoothctl list                 rc=124  (killed)  no output
//! bluetoothctl show                 rc=124  (killed)  no output
//! bluetoothctl devices Connected    rc=124  (killed)  no output
//! bluetoothctl devices Paired       rc=124  (killed)  no output
//! ```
//!
//! It is not a parse-surface problem — there is no output to parse.
//! `bluetoothctl` is a readline client that waits for `org.bluez` to
//! appear on the bus, and when nothing is going to bring it up it
//! waits for as long as you let it. Omarchy's own
//! `/usr/bin/omarchy-bluetooth-power` knows this: every single call it
//! makes is wrapped in `timeout 2s`, without exception.
//!
//! **The dock had no such wrapper, and that is why this matters.**
//! `chonk-shell`'s `BackgroundCommand` sampler was a bare
//! `Command::new(program).args(&args).output()` on a worker thread —
//! no timeout, no kill, no reaping deadline — and a `Source::Command`
//! pointed at `bluetoothctl` on this machine would have wedged that
//! worker *permanently* on its first run: the source never producing
//! another reading, the thread never returning, the child never
//! reaped. `run_detached`, the `Effect::Run` executor, had the same
//! shape and leaked a thread and a process per click. Reporting that
//! is what put a deadline and a kill on both (8s for a sampler, whose
//! run has already missed its interval by then; 120s for an effect,
//! which may be a dialog a human is typing into), and a killed
//! sampler run now reads as *no reading*, so its widget draws a dead
//! face rather than a stale number.
//!
//! That fixes the wedge; it does not make `bluetoothctl` a sampler.
//! A source whose every run is killed at the deadline is not a
//! source — it is a stall with a timer on it, and it still costs a
//! process and eight seconds of a worker per interval. This is the
//! 2026-08-29 incident class that this crate's entire architecture —
//! the `Source`/`Effect` split, the `clippy.toml`, the dependency
//! list — exists to make structurally unavailable, and walking back
//! into it through the one tool that fails this way would be a poor
//! trade for a friendlier command name.
//!
//! So **chonkstep never execs `bluetoothctl`**, for reads or for
//! writes. `busctl` is the sampler and the actuator, and it behaves
//! exactly as a sampler must on the same machine:
//!
//! ```text
//! busctl --system --json=short call org.bluez /org/bluez \
//!        org.freedesktop.DBus.ObjectManager GetManagedObjects
//!   -> rc=1, ~0s, "Call failed: Could not activate remote peer 'org.bluez': unit failed"
//! ```
//!
//! Non-zero in about no time, which the sampler already reads
//! correctly: `BackgroundCommand` keeps a reading only when
//! `status.success()`, so a failed call clears the slot,
//! [`Samples::text`] answers `None`, and the widget draws the face for
//! "BlueZ is not answering". It also did *not* leave `bluetoothd`
//! running afterwards, so sampling once a second does not turn into a
//! daemon this desk never asked for.
//!
//! [`Samples::text`]: chonk_dock_widget::Samples::text
//!
//! # Adapter presence is a sysfs question, not a BlueZ one
//!
//! Whether hardware exists and whether a daemon is answering are
//! different questions with different honest answers, and folding them
//! together would let a stopped `bluetooth.service` render as "you own
//! no Bluetooth hardware". So presence comes from `/sys/class/bluetooth`
//! through a [`Source::Tree`] — the same shape, and for the same
//! reason, as [`crate::WifiWidget`]'s `/sys/class/net` walk: a
//! filesystem read cannot hang on an absent daemon, and it picks up a
//! USB dongle on the sample after it is plugged in.
//!
//! [`Source::Tree`]: chonk_dock_widget::Source::Tree

use super::json::Json;

/// The D-Bus interface names this module reads. Spelled once, here,
/// because a typo in one of them is a silently empty device list
/// rather than an error.
const ADAPTER_IFACE: &str = "org.bluez.Adapter1";
const DEVICE_IFACE: &str = "org.bluez.Device1";
const BATTERY_IFACE: &str = "org.bluez.Battery1";

/// One adapter, as BlueZ reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Adapter {
    /// The D-Bus object path, e.g. `/org/bluez/hci0` — carried because
    /// every action against this adapter (powering it, removing a
    /// device) needs it as an argv element.
    pub path: String,
    pub powered: bool,
}

/// One known device: paired, connected, or both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// `/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF` — the runtime argument
    /// every per-device action carries.
    pub path: String,
    /// The name to show: `Alias` when BlueZ has one (it is the
    /// user-renamable field and the one `bluetoothctl` prints), else
    /// `Name`, else the address recovered from the object path — a
    /// device with no name at all must still be forgettable.
    pub name: String,
    pub connected: bool,
    pub paired: bool,
    /// BlueZ's `Icon` property: `audio-headset`, `input-keyboard`,
    /// `phone`, and so on. `None` when the device does not publish one.
    pub icon: Option<String>,
    /// `org.bluez.Battery1`'s `Percentage`, for the devices that expose
    /// it (most modern headsets do, most mice do not).
    pub battery: Option<u8>,
}

/// Everything one `GetManagedObjects` reply said, in the order BlueZ
/// listed it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BluezState {
    pub adapters: Vec<Adapter>,
    pub devices: Vec<Device>,
}

impl BluezState {
    /// The adapter a click acts on: the first powered one, else the
    /// first at all. Mirrors [`crate::WifiWidget`]'s `best_probe` —
    /// and, more importantly, mirrors what
    /// `omarchy-bluetooth-power`'s `powered()` means by "any
    /// controller counts", so a desk with a powered dongle behind a
    /// dark built-in controller reads as on.
    pub fn primary(&self) -> Option<&Adapter> {
        self.adapters.iter().find(|adapter| adapter.powered).or_else(|| self.adapters.first())
    }

    /// Whether any adapter is powered. The all-or-nothing reading,
    /// deliberately: the rfkill block a click toggles is itself
    /// all-or-nothing across every radio, so the state it is read
    /// against has to be too.
    pub fn any_powered(&self) -> bool {
        self.adapters.iter().any(|adapter| adapter.powered)
    }

    /// Connected devices, in BlueZ's order.
    pub fn connected(&self) -> impl Iterator<Item = &Device> {
        self.devices.iter().filter(|device| device.connected)
    }

    /// Paired-but-not-connected devices — the panel's second section,
    /// and the ones whose click means "connect".
    pub fn paired_idle(&self) -> impl Iterator<Item = &Device> {
        self.devices.iter().filter(|device| device.paired && !device.connected)
    }
}

/// The address embedded in a BlueZ device object path, as a MAC:
/// `/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF` becomes `AA:BB:CC:DD:EE:FF`.
///
/// This is the last-resort display name. BlueZ synthesizes the path
/// from the address, so it is always there even when a device has
/// published neither `Alias` nor `Name` — which is exactly the
/// just-appeared device someone most wants to be able to see.
pub fn address_from_path(path: &str) -> Option<String> {
    let tail = path.rsplit('/').next()?;
    let body = tail.strip_prefix("dev_")?;
    let mut parts = Vec::new();
    for part in body.split('_') {
        if part.len() != 2 || !part.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        parts.push(part);
    }
    (parts.len() == 6).then(|| parts.join(":"))
}

/// A property out of a `busctl --json=short` `a{sv}` bag: each value is
/// itself wrapped in `{"type": ..., "data": ...}`, so reading one is
/// always a two-step.
fn property<'a>(bag: &'a Json, name: &str) -> Option<&'a Json> {
    bag.get(name)?.get("data")
}

/// Parses one `GetManagedObjects` reply.
///
/// The reply's shape is `a{oa{sa{sv}}}` — a map from object path, to a
/// map from interface name, to that interface's property bag — which
/// `busctl --json=short` renders as
/// `{"type":"a{oa{sa{sv}}}","data":[{ "<path>": { "<iface>": { "<prop>": {"type":_,"data":_} } } }]}`.
/// The `data` array holding exactly one element is busctl's encoding of
/// a single return value, not a list of replies.
///
/// Unparseable input yields `None` and a *partially* strange reply
/// yields whatever made sense: an object with no recognized interface
/// is skipped, a device missing `Paired` reads as unpaired. BlueZ's
/// property set grows between releases, and an instrument that blanked
/// itself over an unknown key would be choosing the worst moment to be
/// pedantic.
pub fn parse_managed_objects(output: &str) -> Option<BluezState> {
    let reply = Json::parse(output)?;
    let objects = reply.get("data")?.as_array()?.first()?;
    let mut state = BluezState::default();

    for (path, interfaces) in objects.entries() {
        if let Some(adapter) = interfaces.get(ADAPTER_IFACE) {
            state.adapters.push(Adapter {
                path: path.clone(),
                powered: property(adapter, "Powered").and_then(Json::as_bool).unwrap_or(false),
            });
        }
        let Some(device) = interfaces.get(DEVICE_IFACE) else { continue };
        // `Alias` before `Name`: it is the field BlueZ lets a user
        // rename and the one `bluetoothctl devices` prints, so a device
        // someone has already renamed must show the name they chose.
        let name = property(device, "Alias")
            .and_then(Json::as_str)
            .filter(|alias| !alias.trim().is_empty())
            .or_else(|| property(device, "Name").and_then(Json::as_str))
            .map(str::to_string)
            .or_else(|| address_from_path(path))
            .unwrap_or_else(|| "UNKNOWN".to_string());
        state.devices.push(Device {
            path: path.clone(),
            name,
            connected: property(device, "Connected").and_then(Json::as_bool).unwrap_or(false),
            paired: property(device, "Paired").and_then(Json::as_bool).unwrap_or(false),
            icon: property(device, "Icon").and_then(Json::as_str).map(str::to_string),
            battery: interfaces.get(BATTERY_IFACE).and_then(|bag| property(bag, "Percentage")).and_then(Json::as_u8),
        });
    }
    Some(state)
}

/// The rfkill soft/hard block over Bluetooth, read from
/// `/sys/class/rfkill` rather than from `rfkill(1)`.
///
/// This is the state that *persists*, and
/// `omarchy-bluetooth-power`'s header is the argument for why the
/// instrument has to know it: BlueZ never saves an adapter's `Powered`
/// property, so turning Bluetooth off through D-Bus lasts until the
/// next boot, while `systemd-rfkill` saves and restores every switch
/// under `/var/lib/systemd/rfkill` as its entire job. The block is
/// also all-or-nothing across every radio, where a `Powered` write
/// only ever addresses one adapter.
///
/// The practical consequence, and the reason a click has to check this
/// first: **a plain power-on fails outright while the soft block is
/// set.** Unblocking is the move, and with BlueZ's stock `AutoEnable`
/// the daemon then powers the adapter up by itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RfkillState {
    /// A Bluetooth rfkill switch exists at all.
    pub present: bool,
    /// Soft-blocked: software said no. Clearable with `rfkill unblock`.
    pub soft: bool,
    /// Hard-blocked: a physical switch or firmware said no. **Nothing
    /// this desktop can run will clear it**, which is why the panel
    /// shows it rather than offering a button that cannot work.
    pub hard: bool,
}

impl RfkillState {
    /// Whether software is what is standing in the way — the one case
    /// where `rfkill unblock bluetooth` is the right click.
    pub fn soft_blocked(&self) -> bool {
        self.present && self.soft && !self.hard
    }
}

/// Folds a `/sys/class/rfkill` walk down to the Bluetooth switches.
///
/// Positional against the source's declared files, exactly as
/// [`crate::WifiWidget`]'s `probes_from` is: `type`, `soft`, `hard`.
/// Non-Bluetooth switches (the wifi radio, which is usually right
/// beside it) are skipped by `type`, and blocks are OR-ed across every
/// Bluetooth switch because the block a click sets applies to all of
/// them.
pub fn rfkill_from(entries: &[chonk_dock_widget::TreeEntry]) -> RfkillState {
    let mut state = RfkillState::default();
    for entry in entries {
        if entry.file(0).map(str::trim) != Some("bluetooth") {
            continue;
        }
        state.present = true;
        state.soft |= entry.file(1).map(str::trim) == Some("1");
        state.hard |= entry.file(2).map(str::trim) == Some("1");
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;
    use chonk_dock_widget::TreeEntry;

    /// A `GetManagedObjects` reply in the exact envelope
    /// `busctl --json=short` produces. The envelope
    /// (`{"type":...,"data":[ ... ]}`, and the `{"type":_,"data":_}`
    /// wrapper around every property value) is not recalled — it is the
    /// shape captured live from this machine's systemd via
    /// `Properties.GetAll`, which returns the same `a{sv}` BlueZ does.
    /// The BlueZ *contents* are canned, because this machine has no
    /// adapter; see the module doc.
    fn reply(objects: &str) -> String {
        format!(r#"{{"type":"a{{oa{{sa{{sv}}}}}}","data":[{objects}]}}"#)
    }

    fn full_reply() -> String {
        reply(
            r#"{
              "/org/bluez/hci0": {
                "org.bluez.Adapter1": {
                  "Address": {"type":"s","data":"00:1A:7D:DA:71:13"},
                  "Powered": {"type":"b","data":true},
                  "Discovering": {"type":"b","data":false}
                }
              },
              "/org/bluez/hci0/dev_F8_4E_17_00_11_22": {
                "org.bluez.Device1": {
                  "Alias": {"type":"s","data":"WH-1000XM4"},
                  "Name": {"type":"s","data":"WH-1000XM4"},
                  "Connected": {"type":"b","data":true},
                  "Paired": {"type":"b","data":true},
                  "Icon": {"type":"s","data":"audio-headset"}
                },
                "org.bluez.Battery1": {
                  "Percentage": {"type":"y","data":80}
                }
              },
              "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF": {
                "org.bluez.Device1": {
                  "Alias": {"type":"s","data":"MX Keys"},
                  "Connected": {"type":"b","data":false},
                  "Paired": {"type":"b","data":true},
                  "Icon": {"type":"s","data":"input-keyboard"}
                }
              }
            }"#,
        )
    }

    #[test]
    fn a_full_reply_yields_the_adapter_and_both_devices() {
        let state = parse_managed_objects(&full_reply()).expect("parses");
        assert_eq!(state.adapters, vec![Adapter { path: "/org/bluez/hci0".into(), powered: true }]);
        assert!(state.any_powered());
        assert_eq!(state.devices.len(), 2);

        let headset = &state.devices[0];
        assert_eq!(headset.name, "WH-1000XM4");
        assert!(headset.connected && headset.paired);
        assert_eq!(headset.icon.as_deref(), Some("audio-headset"));
        assert_eq!(headset.battery, Some(80), "Battery1 is a separate interface on the same object");

        let keyboard = &state.devices[1];
        assert_eq!(keyboard.name, "MX Keys");
        assert!(!keyboard.connected && keyboard.paired);
        assert_eq!(keyboard.battery, None, "a device with no Battery1 reports no battery, not zero");
    }

    #[test]
    fn the_sections_split_by_connection() {
        let state = parse_managed_objects(&full_reply()).expect("parses");
        assert_eq!(state.connected().map(|d| d.name.as_str()).collect::<Vec<_>>(), vec!["WH-1000XM4"]);
        assert_eq!(state.paired_idle().map(|d| d.name.as_str()).collect::<Vec<_>>(), vec!["MX Keys"]);
    }

    #[test]
    fn an_unpowered_adapter_with_no_devices_is_still_an_adapter() {
        let state = parse_managed_objects(&reply(
            r#"{"/org/bluez/hci0":{"org.bluez.Adapter1":{"Powered":{"type":"b","data":false}}}}"#,
        ))
        .expect("parses");
        assert_eq!(state.adapters.len(), 1);
        assert!(!state.any_powered());
        assert!(state.devices.is_empty());
        assert_eq!(state.primary().map(|a| a.path.as_str()), Some("/org/bluez/hci0"));
    }

    /// The "any controller counts" rule `omarchy-bluetooth-power`
    /// spells out: a powered dongle behind a dark built-in reads as on,
    /// and is the adapter a click addresses.
    #[test]
    fn a_powered_dongle_behind_a_dark_controller_is_the_primary() {
        let state = parse_managed_objects(&reply(
            r#"{
              "/org/bluez/hci0":{"org.bluez.Adapter1":{"Powered":{"type":"b","data":false}}},
              "/org/bluez/hci1":{"org.bluez.Adapter1":{"Powered":{"type":"b","data":true}}}
            }"#,
        ))
        .expect("parses");
        assert!(state.any_powered());
        assert_eq!(state.primary().map(|a| a.path.as_str()), Some("/org/bluez/hci1"));
    }

    #[test]
    fn a_nameless_device_falls_back_to_its_address() {
        let state = parse_managed_objects(&reply(
            r#"{"/org/bluez/hci0/dev_01_23_45_67_89_AB":{"org.bluez.Device1":{"Paired":{"type":"b","data":true}}}}"#,
        ))
        .expect("parses");
        assert_eq!(state.devices[0].name, "01:23:45:67:89:AB", "a nameless device must still be identifiable");
    }

    #[test]
    fn an_empty_alias_falls_through_to_name() {
        let state = parse_managed_objects(&reply(
            r#"{"/org/bluez/hci0/dev_01_23_45_67_89_AB":{"org.bluez.Device1":{
                 "Alias":{"type":"s","data":"  "},"Name":{"type":"s","data":"Trackball"}}}}"#,
        ))
        .expect("parses");
        assert_eq!(state.devices[0].name, "Trackball");
    }

    /// BlueZ grows properties between releases. An instrument that
    /// blanked itself over an unfamiliar key would pick the worst
    /// possible moment to be strict.
    #[test]
    fn unknown_interfaces_and_properties_are_ignored_not_fatal() {
        let state = parse_managed_objects(&reply(
            r#"{
              "/org/bluez/hci0":{"org.bluez.AdvertisementMonitorManager1":{},"org.bluez.Adapter1":{"Powered":{"type":"b","data":true},"SomethingNew":{"type":"s","data":"?"}}},
              "/org/bluez/hci0/dev_01_23_45_67_89_AB":{"org.bluez.Device1":{"Alias":{"type":"s","data":"Buds"},"Connected":{"type":"b","data":true},"FutureField":{"type":"u","data":7}}}
            }"#,
        ))
        .expect("parses");
        assert!(state.any_powered());
        assert_eq!(state.devices[0].name, "Buds");
        assert!(!state.devices[0].paired, "a missing Paired reads as unpaired, not as an error");
    }

    #[test]
    fn an_empty_bluez_is_an_empty_state_not_a_failure() {
        let state = parse_managed_objects(&reply("{}")).expect("parses");
        assert_eq!(state, BluezState::default());
        assert!(state.primary().is_none());
    }

    /// What the development host actually produces. `busctl` writes its
    /// diagnostic to stderr and exits 1, so the sampler clears the slot
    /// and the widget never sees this string — but if the shape of that
    /// ever changes, it must still not parse into a confident answer.
    #[test]
    fn a_busctl_failure_message_never_parses_into_a_state() {
        for junk in [
            "Call failed: Could not activate remote peer 'org.bluez': unit failed",
            "",
            "{}",
            r#"{"type":"a{oa{sa{sv}}}"}"#,
            r#"{"type":"a{oa{sa{sv}}}","data":[]}"#,
        ] {
            assert_eq!(parse_managed_objects(junk), None, "{junk:?} must not become a device list");
        }
    }

    #[test]
    fn device_addresses_come_back_out_of_their_object_paths() {
        assert_eq!(address_from_path("/org/bluez/hci0/dev_F8_4E_17_00_11_22").as_deref(), Some("F8:4E:17:00:11:22"));
        assert_eq!(address_from_path("/org/bluez/hci3/dev_aa_bb_cc_dd_ee_ff").as_deref(), Some("aa:bb:cc:dd:ee:ff"));
        for bad in ["/org/bluez/hci0", "/org/bluez/hci0/dev_F8_4E", "/org/bluez/hci0/dev_ZZ_4E_17_00_11_22", "", "dev_"] {
            assert_eq!(address_from_path(bad), None, "{bad:?} is not a device path");
        }
    }

    fn switch(kind: &str, soft: &str, hard: &str) -> TreeEntry {
        TreeEntry {
            name: "rfkill0".to_string(),
            files: vec![Some(format!("{kind}\n")), Some(format!("{soft}\n")), Some(format!("{hard}\n"))],
            dirs: Vec::new(),
        }
    }

    #[test]
    fn rfkill_reads_only_the_bluetooth_switches() {
        let state = rfkill_from(&[switch("wlan", "1", "0"), switch("bluetooth", "0", "0")]);
        assert_eq!(state, RfkillState { present: true, soft: false, hard: false });
        assert!(!state.soft_blocked(), "the wifi radio's block is not Bluetooth's");
    }

    #[test]
    fn a_soft_block_is_the_one_a_click_can_clear() {
        let soft = rfkill_from(&[switch("bluetooth", "1", "0")]);
        assert!(soft.soft_blocked(), "software said no, so software can say yes");

        let hard = rfkill_from(&[switch("bluetooth", "1", "1")]);
        assert!(!hard.soft_blocked(), "a hardware switch is not ours to flip, so never offer to");

        let clear = rfkill_from(&[switch("bluetooth", "0", "0")]);
        assert!(!clear.soft_blocked());
    }

    /// Blocks OR across switches because the block a click sets is
    /// all-or-nothing across the radios — the same reason
    /// `omarchy-bluetooth-power` reads power across every controller.
    #[test]
    fn blocks_or_across_multiple_bluetooth_switches() {
        let state = rfkill_from(&[switch("bluetooth", "0", "0"), switch("bluetooth", "1", "0")]);
        assert!(state.soft, "one blocked switch blocks");
        assert!(state.soft_blocked());
    }

    /// The development host: `/sys/class/rfkill` exists but holds no
    /// Bluetooth switch at all.
    #[test]
    fn no_bluetooth_switch_at_all_is_not_a_block() {
        let state = rfkill_from(&[switch("wlan", "0", "0")]);
        assert_eq!(state, RfkillState { present: false, soft: false, hard: false });
        assert!(!state.soft_blocked());
        assert!(!rfkill_from(&[]).present, "an empty walk is an absent switch, not a blocked one");
    }
}
