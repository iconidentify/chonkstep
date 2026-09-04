//! The BT tile's fold-out: what Bluetooth this machine has, the
//! devices it knows, and the door to the pairing dialog.
//!
//! Same discipline as every instrument in this crate. The panel is a
//! pure fold: it is fed already-sampled state by [`crate::BluetoothWidget`],
//! turns it into a [`crate::bt_panel::render::BtView`] the renderer draws, and answers input
//! with a [`chonk_dock_widget::PanelReaction`] instead of performing anything. No entry
//! point here can reach a syscall — the crate's `clippy.toml` makes
//! that a build error and its dependency list makes it moot.
//!
//! The host is [`crate::BluetoothWidget`], through the panel half of
//! the `DockWidget` trait (`panel_spec` / `render_panel` /
//! `panel_input` / `panel_tick`); this type is the brain those four
//! methods delegate to. Drawing is [`crate::bt_panel::render`], which also owns the
//! band geometry ([`crate::bt_panel::render::bt_layout`]) that the hit test below asks,
//! so the pixels and the pointer cannot disagree about where a cell is.
//!
//! # The three absences
//!
//! Most desks running this instrument have no Bluetooth at all, and
//! there are three different ways for that to be true. They used to
//! render alike — one row saying `NO ADAPTER`, in a panel 50 pixels
//! tall — and the whole point of [`crate::bt_panel::render::BtStatus`] having three absent
//! variants is that they are three different truths with three
//! different remedies:
//!
//! | Reading | Status | What the panel offers |
//! |---|---|---|
//! | `/sys/class/bluetooth` empty | [`crate::bt_panel::render::BtStatus::NoRadio`] | nothing: there is no radio to act on, so there is no control to draw |
//! | a controller in sysfs, no adapter in BlueZ's reply | [`crate::bt_panel::render::BtStatus::NoDaemon`] | the command that starts the service — plus an unblock, but only if rfkill is what is standing in the way |
//! | BlueZ answering, no adapter powered | [`crate::bt_panel::render::BtStatus::Off`] | the power row, and the known devices in the disabled treatment |
//!
//! The sysfs walk and the BlueZ call are deliberately different
//! questions — see [`crate::bt_panel::bluez`]'s module doc — and this is where the
//! difference is finally *shown* rather than only measured.
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
//! a fact. The truth is the next sample: every [`chonk_dock_widget::Effect::Run`] here
//! sets `then:` to the BlueZ source that can confirm it, a pending row
//! reconciles the moment that sample agrees, and a pending that
//! outlives [`crate::bt_panel::PENDING_DEADLINE_SAMPLES`] fresh samples reverts to
//! showing reality — because an instrument still saying "connecting…"
//! after the system gave up is lying with extra steps.
//!
//! # Two clicks to forget, and why not a long press
//!
//! Forgetting a device is destructive and unrecoverable from this
//! panel: the pairing keys go with it, and getting the device back
//! means pairing it again, in person, with the device in pairing mode.
//! It therefore wants a confirmation. The panel input vocabulary
//! ([`chonk_dock_widget::PanelEvent`]) has no long-press — it is press, release, scroll,
//! motion and crossings, and a panel takes no keyboard *ever*, by
//! design — so the confirm is two clicks on the same `[x]` within
//! [`crate::bt_panel::FORGET_GRACE`]. The first arms it, and the *row* becomes the
//! question — `FORGET?` in lit ink where the battery reading was, the
//! cell inverted beside it — so the pending question is on the face
//! rather than in someone's memory; the second commits; anything else
//! disarms it.

pub mod bluez;
pub mod json;
pub mod render;

use std::time::{Duration, Instant};

use chonk_dock_widget::{Effect, PanelEvent, PanelReaction, PanelSpec, SourceId};

use bluez::{BluezState, RfkillState};
use render::{Block, BtLayout, BtRowKey, BtStatus, BtView, DeviceRow};

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
    /// The controller sysfs named (`hci0`), if any. Deliberately not
    /// the same question as whether BlueZ is answering — see
    /// [`bluez`]'s module doc — and the plate for a silent daemon
    /// names it, because "the hardware is real, the software is not
    /// running" is the whole of that state.
    controller: Option<String>,
    powered: bool,
    /// Whether BlueZ answered at all this pass. An adapter list that
    /// is empty while sysfs has a controller *is* the silent daemon.
    daemon: bool,
    /// The adapter every adapter-scoped action addresses.
    adapter: Option<String>,
    rfkill: RfkillState,
    devices: Vec<bluez::Device>,
    pending: Vec<Pending>,
    /// The `[x]` waiting for its second click, and when it was armed.
    armed: Option<(String, Instant)>,
    hover: Option<BtRowKey>,
    pressed: Option<BtRowKey>,
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
    /// The last view rendered, kept only to notice that the next one
    /// differs — the panel repaints on change, never on a timer.
    last_view: Option<BtView>,
}

impl BtPanel {
    pub fn new() -> Self {
        Self {
            controller: None,
            powered: false,
            daemon: false,
            adapter: None,
            rfkill: RfkillState::default(),
            devices: Vec::new(),
            pending: Vec::new(),
            armed: None,
            hover: None,
            pressed: None,
            last_tick: None,
            bluez_src: SourceId::UNBOUND,
            dirty: true,
            last_view: None,
        }
    }

    pub fn bind(&mut self, bluez_src: SourceId) {
        self.bluez_src = bluez_src;
    }

    /// Feeds the panel what the widget already sampled, so the panel
    /// never re-reads what the tile has folded. `controller` is the
    /// sysfs name of the first controller (`None` when there is no
    /// Bluetooth hardware at all) and `fresh` is whether this pass
    /// carried a new BlueZ reading — the thing a pending's deadline is
    /// measured in.
    pub fn set_state(&mut self, controller: Option<&str>, state: &BluezState, rfkill: RfkillState, fresh: bool) {
        self.controller = controller.map(str::to_string);
        self.powered = state.any_powered();
        self.daemon = !state.adapters.is_empty();
        self.adapter = state.primary().map(|adapter| adapter.path.clone());
        self.rfkill = rfkill;
        self.devices = state.devices.clone();
        if fresh {
            self.reconcile();
        }
        self.note_change();
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

    /// The reading, as one of the four truths. The order is the order
    /// the questions were asked in: hardware, then daemon, then power.
    fn status(&self) -> BtStatus {
        if self.controller.is_none() {
            return BtStatus::NoRadio;
        }
        if !self.daemon {
            return BtStatus::NoDaemon;
        }
        if !self.powered {
            let block = if self.rfkill.hard {
                Block::Hard
            } else if self.rfkill.soft_blocked() {
                Block::Soft
            } else {
                Block::None
            };
            return BtStatus::Off { block };
        }
        let connected = self.devices.iter().filter(|device| device.connected).count();
        BtStatus::On { connected: connected.min(u8::MAX as usize) as u8 }
    }

    /// Everything the renderer draws. The row order is the panel's
    /// grammar: what is connected, then what is merely known.
    pub fn view(&self) -> BtView {
        let status = self.status();
        let mut devices = Vec::new();
        if !matches!(status, BtStatus::NoRadio | BtStatus::NoDaemon) {
            let mut push = |device: &bluez::Device| {
                let armed = self.armed.as_ref().is_some_and(|(path, _)| path == &device.path);
                devices.push(DeviceRow::from_device(device, self.is_pending(&device.path), armed));
            };
            for device in self.devices.iter().filter(|device| device.connected) {
                push(device);
            }
            for device in self.devices.iter().filter(|device| device.paired && !device.connected) {
                push(device);
            }
        }
        BtView {
            status,
            controller: self.controller.clone(),
            devices,
            hover: self.hover.clone(),
            pressed: self.pressed.clone(),
        }
    }

    /// Marks the panel dirty when the face it would draw has actually
    /// changed. Everything the renderer reads lives in [`BtView`], so
    /// comparing views is exactly comparing pixels — and an open panel
    /// that repainted on every sample would cost a frame a second for
    /// a screen that did not move.
    fn note_change(&mut self) {
        let view = self.view();
        if self.last_view.as_ref() != Some(&view) {
            self.last_view = Some(view);
            self.dirty = true;
        }
    }

    fn layout(&self, tile: u32) -> BtLayout {
        render::bt_layout(&self.view(), tile)
    }

    pub fn spec(&self, tile: u32) -> PanelSpec {
        let layout = self.layout(tile);
        PanelSpec::new(layout.width, layout.height)
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
        let view = self.view();
        let buffer = render::render_bt_panel_into(theme, fonts, swash, tile, &view, width, height);
        self.last_view = Some(view);
        buffer
    }

    /// Whether the panel's pixels changed since the last render.
    pub fn tick(&mut self, now: Instant) -> bool {
        self.last_tick = Some(now);
        // An arming that ran out of grace disarms itself, which is a
        // visible change: the row stops asking its question.
        if let Some((_, armed_at)) = self.armed {
            if now.duration_since(armed_at) >= FORGET_GRACE {
                self.armed = None;
                self.dirty = true;
            }
        }
        std::mem::take(&mut self.dirty)
    }

    /// One panel event. Press highlights, release fires — the idiom
    /// the LNK and SND panels already keep, so a pointer means the
    /// same thing in all three fold-outs. It also makes the forget
    /// confirm honest: a press that slides off its cell before the
    /// release neither arms nor commits anything.
    pub fn on_event(&mut self, event: PanelEvent, tile: u32) -> PanelReaction {
        let layout = self.layout(tile);
        match event {
            PanelEvent::Enter => PanelReaction::None,
            PanelEvent::Scroll { .. } => PanelReaction::None,
            PanelEvent::Motion { local } => {
                let over = layout.row_at(local.x, local.y);
                if over == self.hover {
                    return PanelReaction::None;
                }
                self.hover = over;
                PanelReaction::Repaint
            }
            PanelEvent::Leave => {
                if self.hover.is_none() && self.pressed.is_none() {
                    return PanelReaction::None;
                }
                self.hover = None;
                self.pressed = None;
                PanelReaction::Repaint
            }
            PanelEvent::LeftPress { local } => {
                self.pressed = layout.row_at(local.x, local.y);
                // A press on the frame's furniture is still a click
                // "somewhere else", and somewhere else disarms a
                // pending confirm.
                if self.pressed.is_none() {
                    return self.disarm();
                }
                PanelReaction::Repaint
            }
            PanelEvent::LeftRelease { local } => {
                let target = layout.row_at(local.x, local.y);
                let had_highlight = self.pressed.is_some();
                let fired = self.pressed.take().filter(|key| Some(key) == target.as_ref());
                let Some(key) = fired else {
                    // A press that changed its mind before the release
                    // performs nothing — and, like any other click that
                    // is not the armed cell's second, disarms.
                    let disarmed = self.armed.take().is_some();
                    return if had_highlight || disarmed { PanelReaction::Repaint } else { PanelReaction::None };
                };
                self.activate(&key)
            }
        }
    }

    /// The action table, applied to one released row.
    fn activate(&mut self, key: &BtRowKey) -> PanelReaction {
        // Any click that is not the armed cell's second click cancels
        // the arming — including a click on a *different* row's `[x]`,
        // which then arms that one instead.
        let armed = self.armed.take();
        let reaction = match key {
            BtRowKey::Power => self.power_action(),
            BtRowKey::PairNew => {
                // No `then:`. The dialog is a window someone is about
                // to spend a minute in; the sample that matters lands
                // long after it exits, on the panel's own cadence.
                PanelReaction::Run(Effect::Run { program: PAIR_DIALOG, args: Vec::new(), then: None })
            }
            BtRowKey::Device(path) => {
                let connected = self.devices.iter().find(|device| &device.path == path).map(|device| device.connected);
                match connected {
                    Some(connected) => self.toggle_device(path, connected),
                    None => PanelReaction::None,
                }
            }
            BtRowKey::Forget(path) => {
                let now = self.last_tick;
                let confirmed = armed.as_ref().is_some_and(|(armed_path, at)| {
                    armed_path == path && now.is_none_or(|now| now.duration_since(*at) < FORGET_GRACE)
                });
                if confirmed {
                    self.dirty = true;
                    return self.forget(path);
                }
                // Arm it. Without a tick to date the arming from, arm
                // anyway and let the next tick date it — the panel is
                // ticked every pass while open, so this is the first
                // click of a freshly-opened panel at worst.
                self.armed = now.map(|now| (path.clone(), now));
                self.dirty = true;
                PanelReaction::Repaint
            }
        };
        // A row that declined to fire (a hard block, a device already
        // mid-negotiation) still had a press highlight, and a stale
        // arming this click cancelled is a visible change too — so the
        // release is never free of a repaint once it had a target.
        match reaction {
            PanelReaction::None => PanelReaction::Repaint,
            other => other,
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
