//! The BT tile's fold-out: adapter power, the devices this machine
//! knows, and the door to the pairing dialog.
//!
//! Same discipline as every instrument in this crate. The panel is a
//! pure fold: it is fed already-sampled state by [`crate::BluetoothWidget`],
//! turns it into rows, and answers input with a [`PanelReaction`]
//! instead of performing anything. No entry point here can reach a
//! syscall — the crate's `clippy.toml` makes that a build error and its
//! dependency list makes it moot.
//!
//! The host is [`crate::BluetoothWidget`], through the panel half of
//! the `DockWidget` trait (`panel_spec` / `render_panel` /
//! `panel_input` / `panel_tick`); this type is the brain those four
//! methods delegate to. Drawing is [`wm_theme::bluetooth`]'s
//! `render_bt_panel`, which also owns the row geometry
//! ([`panel_row_height`], [`forget_cell_width`]) so the hit-test below
//! and the pixels cannot disagree about where a cell is.
//!
//! # The action table
//!
//! Every argv this panel can ever request, in one place. Programs are
//! compile-time `&'static str` — the executor's whitelist is the
//! compiler — and the only runtime-supplied arguments are D-Bus object
//! paths, which come from BlueZ's own reply rather than from anything
//! typed, and each rides as one argv element, never through a shell.
//!
//! | Row | Action | Argv |
//! |---|---|---|
//! | power, soft-blocked | unblock | `rfkill unblock bluetooth` |
//! | power, off and unblocked | power on | `busctl --system set-property org.bluez <adapter> org.bluez.Adapter1 Powered b true` |
//! | power, on | power off | `rfkill block bluetooth` |
//! | power, hard-blocked | *(inert)* | — |
//! | device, connected | disconnect | `busctl --system call org.bluez <device> org.bluez.Device1 Disconnect` |
//! | device, paired idle | connect | `busctl --system call org.bluez <device> org.bluez.Device1 Connect` |
//! | device `[x]`, confirmed | forget | `busctl --system call org.bluez <adapter> org.bluez.Adapter1 RemoveDevice o <device>` |
//! | pair new | open dialog | `chonk-btpair` |
//!
//! # Why the power row is not one command
//!
//! `omarchy-bluetooth-power` is the reference, and the reason it is
//! three branches rather than a `bluetoothctl power on` is worth
//! restating because it is not obvious: **BlueZ never persists an
//! adapter's `Powered` property**, so powering off through D-Bus lasts
//! until the next boot, while the rfkill soft block *does* persist —
//! `systemd-rfkill` saving and restoring every switch under
//! `/var/lib/systemd/rfkill` is its entire job. The block is also
//! all-or-nothing across every radio, where a `Powered` write
//! addresses one adapter.
//!
//! So the block is the state and BlueZ follows it: unblocking leaves
//! `AutoEnable` at its stock default and `bluetoothd` powers the
//! adapter up on its own. The order matters in the other direction
//! too — **a power-on fails outright while the block is set** — which
//! is why the click reads rfkill before it decides. This is
//! reimplemented natively against `/sys/class/rfkill` and `busctl`
//! rather than executed: chonkstep must not require Omarchy to be
//! installed, so that script is a reference, never a dependency, and
//! is never spawned.
//!
//! # Optimism with a deadline
//!
//! A Bluetooth connect takes seconds — a headset has to be woken,
//! negotiated with, and its profiles brought up. So a clicked row dims
//! and gains an ellipsis immediately, but the toggle is a request, not
//! a fact. The truth is the next sample: every [`Effect::Run`] here
//! sets `then:` to the BlueZ source that can confirm it, a pending row
//! reconciles the moment that sample agrees, and a pending that
//! outlives [`PENDING_DEADLINE_SAMPLES`] fresh samples reverts to
//! showing reality — because an instrument still saying "connecting…"
//! after the system gave up is lying with extra steps.
//!
//! # Two clicks to forget, and why not a long press
//!
//! Forgetting a device is destructive and unrecoverable from this
//! panel: the pairing keys go with it, and getting the device back
//! means pairing it again, in person, with the device in pairing mode.
//! It therefore wants a confirmation. The panel input vocabulary
//! ([`PanelEvent`]) has no long-press — it is press, release, scroll,
//! motion and crossings, and a panel takes no keyboard *ever*, by
//! design — so the confirm is two clicks on the same `[x]` within
//! [`FORGET_GRACE`]. The first arms it, which inverts the cell to full
//! ink so the pending question is visible on the face rather than
//! remembered; the second commits; anything else disarms it.

pub mod bluez;
pub mod json;

use std::time::{Duration, Instant};

use chonk_dock_widget::{Effect, PanelEvent, PanelReaction, PanelSpec, SourceId};
use wm_theme::bluetooth::{forget_cell_width, panel_content_height, panel_content_width, panel_row_height, BtPanelRow};

use bluez::{BluezState, Device, RfkillState};

/// How many fresh BlueZ samples a pending connect or disconnect may
/// outlive before the row goes back to showing reality.
///
/// Four, against the widget's sampling cadence, is comfortably longer
/// than a healthy connect and comfortably shorter than someone's
/// patience with a lie. A device that genuinely took longer reconciles
/// on the sample it does land, because reconciliation is driven by the
/// reading agreeing, not by the deadline expiring.
pub const PENDING_DEADLINE_SAMPLES: u32 = 4;

/// How long an armed `[x]` waits for its second click. Long enough to
/// be a deliberate second press, short enough that an arming forgotten
/// about is gone before the pointer wanders back.
pub const FORGET_GRACE: Duration = Duration::from_secs(3);

/// The panel's own program name for the pairing dialog — a separate
/// process because discovery and passkey confirmation need a window
/// with a lifetime of its own, and a panel is a popover that any click
/// elsewhere dismisses. See the `chonk-btpair` crate.
pub const PAIR_DIALOG: &str = "chonk-btpair";

/// One row's semantics: what it is, and therefore what a click on it
/// means. The rendering twin is [`BtPanelRow`], which is only what a
/// row *looks like*; this is the half that knows which device is which.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Row {
    Power,
    Device { path: String, name: String, connected: bool, battery: Option<u8> },
    PairNew,
    NoAdapter,
}

/// A connect or disconnect that has been asked for and not yet
/// confirmed by a reading.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Pending {
    path: String,
    /// What the click asked the device to become. Reconciliation is
    /// "the sample agrees with this", not "the sample changed".
    want_connected: bool,
    /// Fresh samples remaining before this stops being shown.
    budget: u32,
}

pub struct BtPanel {
    /// Whether any adapter exists at all — the sysfs answer, which is
    /// deliberately not the same question as whether BlueZ is
    /// answering. See [`bluez`]'s module doc.
    present: bool,
    powered: bool,
    /// The adapter every adapter-scoped action addresses.
    adapter: Option<String>,
    rfkill: RfkillState,
    devices: Vec<Device>,
    rows: Vec<Row>,
    pending: Vec<Pending>,
    /// The `[x]` waiting for its second click, and when it was armed.
    armed: Option<(String, Instant)>,
    /// The most recent `panel_tick` time. `panel_input` carries no
    /// clock of its own — [`PanelEvent`] is a pointer vocabulary, not a
    /// timed one — and `panel_tick` is polled every pass while the
    /// panel is open, so this is at most one frame stale: an accurate
    /// enough now for a three-second grace window, and the only one
    /// available without inventing a clock a widget is not allowed to
    /// read.
    last_tick: Option<Instant>,
    /// The BlueZ source to hurry after an action, and the one whose
    /// freshness spends a pending's budget.
    bluez_src: SourceId,
    dirty: bool,
}

impl BtPanel {
    pub fn new() -> Self {
        Self {
            present: false,
            powered: false,
            adapter: None,
            rfkill: RfkillState::default(),
            devices: Vec::new(),
            rows: Vec::new(),
            pending: Vec::new(),
            armed: None,
            last_tick: None,
            bluez_src: SourceId::UNBOUND,
            dirty: true,
        }
    }

    pub fn bind(&mut self, bluez_src: SourceId) {
        self.bluez_src = bluez_src;
    }

    /// Feeds the panel what the widget already sampled, so the panel
    /// never re-reads what the tile has folded. `fresh` is whether this
    /// pass carried a new BlueZ reading — the thing a pending's
    /// deadline is measured in.
    pub fn set_state(&mut self, present: bool, state: &BluezState, rfkill: RfkillState, fresh: bool) {
        self.present = present;
        self.powered = state.any_powered();
        self.adapter = state.primary().map(|adapter| adapter.path.clone());
        self.rfkill = rfkill;
        self.devices = state.devices.clone();
        if fresh {
            self.reconcile();
        }
        self.rebuild();
    }

    /// Drops pendings the reading now agrees with, and spends the
    /// budget of the ones it does not.
    fn reconcile(&mut self) {
        let devices = &self.devices;
        self.pending.retain_mut(|pending| {
            let settled = devices
                .iter()
                .find(|device| device.path == pending.path)
                .map(|device| device.connected == pending.want_connected)
                // A device that vanished from the reply (forgotten, or
                // the adapter went down) has no pending state left to
                // show: the request is over either way.
                .unwrap_or(true);
            if settled {
                return false;
            }
            pending.budget = pending.budget.saturating_sub(1);
            pending.budget > 0
        });
    }

    fn is_pending(&self, path: &str) -> bool {
        self.pending.iter().any(|pending| pending.path == path)
    }

    /// Rebuilds the row list from the current state. The order is the
    /// panel's grammar: power first, then what is connected, then what
    /// is merely known, then the way to add something new.
    fn rebuild(&mut self) {
        let before = std::mem::take(&mut self.rows);
        if !self.present {
            self.rows.push(Row::NoAdapter);
        } else {
            self.rows.push(Row::Power);
            let mut push = |device: &Device| {
                self.rows.push(Row::Device {
                    path: device.path.clone(),
                    name: device.name.clone(),
                    connected: device.connected,
                    battery: device.battery,
                });
            };
            for device in self.devices.iter().filter(|device| device.connected) {
                push(device);
            }
            for device in self.devices.iter().filter(|device| device.paired && !device.connected) {
                push(device);
            }
            self.rows.push(Row::PairNew);
        }
        if self.rows != before {
            self.dirty = true;
        }
    }

    /// The rows as the renderer takes them — the lookup that turns
    /// semantics into appearance, including the two pieces of state the
    /// renderer shows but the row itself does not carry: whether an
    /// action is in flight, and whether this row's `[x]` is armed.
    fn render_rows(&self) -> Vec<BtPanelRow<'_>> {
        self.rows
            .iter()
            .map(|row| match row {
                Row::Power => BtPanelRow::Power { on: self.powered },
                Row::Device { path, name, connected, .. } => BtPanelRow::Device {
                    name,
                    connected: *connected,
                    pending: self.is_pending(path),
                    armed: self.armed.as_ref().is_some_and(|(armed, _)| armed == path),
                },
                Row::PairNew => BtPanelRow::PairNew,
                Row::NoAdapter => BtPanelRow::NoAdapter,
            })
            .collect()
    }

    pub fn spec(&self, tile: u32) -> PanelSpec {
        let row_h = panel_row_height(tile);
        PanelSpec::new(panel_content_width(tile), panel_content_height(row_h, self.rows.len()))
    }

    pub fn render(
        &mut self,
        theme: &wm_theme::Theme,
        tile: u32,
        width: u32,
        height: u32,
        fonts: &mut cosmic_text::FontSystem,
        swash: &mut cosmic_text::SwashCache,
    ) -> wm_theme_api::DecorationBuffer {
        self.dirty = false;
        let rows = self.render_rows();
        wm_theme::bluetooth::render_bt_panel(theme, fonts, swash, width, height, panel_row_height(tile), &rows)
    }

    /// Whether the panel's pixels changed since the last render.
    pub fn tick(&mut self, now: Instant) -> bool {
        self.last_tick = Some(now);
        // An arming that ran out of grace disarms itself, which is a
        // visible change: the inverted cell goes back to a whisper.
        if let Some((_, armed_at)) = self.armed {
            if now.duration_since(armed_at) >= FORGET_GRACE {
                self.armed = None;
                self.dirty = true;
            }
        }
        std::mem::take(&mut self.dirty)
    }

    /// The row a point lands on, and whether it landed on that row's
    /// forget cell. Geometry comes from `wm-theme`'s two helpers, which
    /// are the same ones the renderer draws with.
    fn hit(&self, local: wm_theme_api::Point, tile: u32, width: u32) -> Option<(usize, bool)> {
        let row_h = panel_row_height(tile);
        if local.x < 0 || local.y < 0 || local.x >= width as i32 {
            return None;
        }
        let index = (local.y as u32 / row_h) as usize;
        if index >= self.rows.len() {
            return None;
        }
        let cell = forget_cell_width(row_h);
        Some((index, local.x >= width.saturating_sub(cell) as i32))
    }

    /// A click, already resolved to a row. `tile` and `width` are the
    /// live geometry — the granted width, not the requested one, so the
    /// forget cell is hit-tested where it was actually drawn.
    pub fn input(&mut self, event: PanelEvent, tile: u32, width: u32) -> PanelReaction {
        let PanelEvent::LeftPress { local } = event else { return PanelReaction::None };
        let Some((index, on_forget)) = self.hit(local, tile, width) else {
            return self.disarm();
        };
        // Any click that is not the armed cell's second click cancels
        // the arming — including a click on a *different* row's `[x]`,
        // which then arms that one instead.
        let armed = self.armed.take();
        let row = self.rows[index].clone();

        match row {
            Row::Power => {
                self.dirty |= armed.is_some();
                self.power_action()
            }
            Row::Device { path, connected, .. } if on_forget => {
                let now = self.last_tick;
                let confirmed = armed
                    .as_ref()
                    .is_some_and(|(armed_path, at)| *armed_path == path && now.is_none_or(|now| now.duration_since(*at) < FORGET_GRACE));
                if confirmed {
                    self.dirty = true;
                    return self.forget(&path);
                }
                // Arm it. Without a tick to date the arming from, arm
                // anyway and let the next tick date it — the panel is
                // ticked every pass while open, so this is the first
                // click of a freshly-opened panel at worst.
                self.armed = now.map(|now| (path, now));
                self.dirty = true;
                PanelReaction::Repaint
            }
            Row::Device { path, connected, .. } => {
                self.dirty |= armed.is_some();
                self.toggle_device(&path, connected)
            }
            Row::PairNew => {
                self.dirty |= armed.is_some();
                // No `then:`. The dialog is a window someone is about to
                // spend a minute in; the sample that matters lands long
                // after it exits, on the panel's own cadence.
                PanelReaction::Run(Effect::Run { program: PAIR_DIALOG, args: Vec::new(), then: None })
            }
            Row::NoAdapter => {
                self.dirty |= armed.is_some();
                PanelReaction::None
            }
        }
    }

    fn disarm(&mut self) -> PanelReaction {
        if self.armed.take().is_some() {
            self.dirty = true;
            return PanelReaction::Repaint;
        }
        PanelReaction::None
    }

    /// The power click, in `omarchy-bluetooth-power`'s order and for
    /// its reasons — see the module doc.
    fn power_action(&mut self) -> PanelReaction {
        if self.rfkill.hard {
            // A physical kill switch. Nothing this desktop can run
            // clears it, so the honest answer to the click is to do
            // nothing rather than to run a command that will fail.
            return PanelReaction::None;
        }
        if self.rfkill.soft_blocked() {
            return PanelReaction::Run(Effect::Run {
                program: "rfkill",
                args: vec!["unblock".to_string(), "bluetooth".to_string()],
                then: Some(self.bluez_src),
            });
        }
        if self.powered {
            // Off is the block, not a `Powered` write: it is the half
            // that survives the reboot.
            return PanelReaction::Run(Effect::Run {
                program: "rfkill",
                args: vec!["block".to_string(), "bluetooth".to_string()],
                then: Some(self.bluez_src),
            });
        }
        // Unblocked but dark: `AutoEnable` will not raise an adapter
        // that was powered down without a block, so ask it directly.
        let Some(adapter) = self.adapter.clone() else { return PanelReaction::None };
        PanelReaction::Run(Effect::Run {
            program: "busctl",
            args: set_powered_args(&adapter, true),
            then: Some(self.bluez_src),
        })
    }

    fn toggle_device(&mut self, path: &str, connected: bool) -> PanelReaction {
        if self.is_pending(path) {
            // Already asked. Clicking again would not make it faster
            // and a second Connect on a device mid-negotiation is how a
            // pairing gets confused.
            return PanelReaction::None;
        }
        self.pending.push(Pending { path: path.to_string(), want_connected: !connected, budget: PENDING_DEADLINE_SAMPLES });
        self.dirty = true;
        PanelReaction::Run(Effect::Run {
            program: "busctl",
            args: device_call_args(path, if connected { "Disconnect" } else { "Connect" }),
            then: Some(self.bluez_src),
        })
    }

    fn forget(&mut self, path: &str) -> PanelReaction {
        let Some(adapter) = self.adapter.clone() else { return PanelReaction::None };
        PanelReaction::Run(Effect::Run {
            program: "busctl",
            args: remove_device_args(&adapter, path),
            then: Some(self.bluez_src),
        })
    }
}

impl Default for BtPanel {
    fn default() -> Self {
        Self::new()
    }
}

/// `busctl --system set-property org.bluez <adapter> org.bluez.Adapter1 Powered b <on>`.
///
/// The `b` is D-Bus's boolean type signature, which `busctl` requires
/// in front of the value; this is a typed call, not a string that gets
/// interpreted somewhere later.
pub fn set_powered_args(adapter: &str, on: bool) -> Vec<String> {
    ["--system", "set-property", "org.bluez", adapter, "org.bluez.Adapter1", "Powered", "b", if on { "true" } else { "false" }]
        .iter()
        .map(|arg| (*arg).to_string())
        .collect()
}

/// `busctl --system call org.bluez <device> org.bluez.Device1 <method>` —
/// `Connect` or `Disconnect`, both of which take no arguments.
pub fn device_call_args(device: &str, method: &str) -> Vec<String> {
    ["--system", "call", "org.bluez", device, "org.bluez.Device1", method].iter().map(|arg| (*arg).to_string()).collect()
}

/// `busctl --system call org.bluez <adapter> org.bluez.Adapter1 RemoveDevice o <device>`.
///
/// Removal is the *adapter's* method, not the device's — the device
/// object is what it destroys — and `o` is the object-path type
/// signature for the argument that follows.
pub fn remove_device_args(adapter: &str, device: &str) -> Vec<String> {
    ["--system", "call", "org.bluez", adapter, "org.bluez.Adapter1", "RemoveDevice", "o", device]
        .iter()
        .map(|arg| (*arg).to_string())
        .collect()
}

#[cfg(test)]
mod tests;
