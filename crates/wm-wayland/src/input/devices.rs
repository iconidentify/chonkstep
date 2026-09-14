//! Switching an input device off and on by its exact name.
//!
//! Two sources decide it. A configuration's device rules (`device { … }`,
//! `hl.device({ … })`, and Omarchy's persisted touchpad and touchscreen
//! disables) say what a device should be, and a live `hl.device` request
//! from Omarchy's toggles says what the user has just asked for. The live
//! request is the newer word about its device until the configuration
//! changes what it says about that same device. Omarchy's toggle writes its
//! data file and sends the request together, so the two agree whichever is
//! read first, and a reload for some unrelated edit never undoes a toggle.
//!
//! A disabled device is switched off three ways. libinput stops sending its
//! events (send-events, in `session`, applied as each device appears, so
//! hotplug and resume keep it off). Anything already queued, and every event
//! on a backend with no libinput under it, is dropped at the input funnel
//! before it can count as activity, so a palm on a switched-off touchpad
//! neither moves the pointer nor wakes the screen. And whatever the device
//! was holding as it went off, a pressed button, a touch or a swipe, is
//! released at the seat, because a device that sends nothing can never send
//! that release itself.
//!
//! A device with keys is never switched off, whichever source asks: a
//! combined keyboard and pointer disabled at the lock screen would leave no
//! way to type the password.

use std::collections::BTreeMap;

use smithay::backend::input::{
    ButtonState, Device, DeviceCapability, Event, InputBackend, InputEvent, MouseButton as InputMouseButton,
    PointerButtonEvent, TouchEvent, TouchSlot,
};
use wm_core::DeviceRule;

use crate::state::Compositor;

/// The most live requests remembered, the bound a configuration's rules have.
const MAX_REQUESTS: usize = DeviceRule::MAX_RULES;

/// The most buttons and touches remembered across every device. A device
/// unplugged mid-click never reports its release, so the ledger forgets its
/// oldest entry rather than growing.
const MAX_HOLDS: usize = 64;

/// Which devices are switched off, and why.
#[derive(Debug, Default)]
pub(crate) struct DeviceStates {
    /// The configuration's own rules, as last configured.
    configured: Vec<DeviceRule>,
    /// Live requests by device name.
    requested: BTreeMap<String, bool>,
    /// Every name switched off now. Kept current on each change, so the
    /// input funnel reads a list rather than rebuilding one per event.
    disabled: Vec<String>,
    /// The `disabled` list as it stood when holds were last released.
    released: Vec<String>,
}

impl DeviceStates {
    /// Adopts a configuration's rules. A live request survives unless the
    /// configuration now says something different about its device than it
    /// did before, which makes the configuration the newer word.
    pub(crate) fn configure(&mut self, rules: &[DeviceRule]) {
        let previous = std::mem::replace(&mut self.configured, rules.to_vec());
        self.requested.retain(|name, _| enabled_in(&previous, name) == enabled_in(rules, name));
        self.refresh();
    }

    /// Records a live request, refused only when the bound is already full
    /// of requests for other devices.
    pub(crate) fn request(&mut self, name: &str, enabled: bool) -> bool {
        if !self.requested.contains_key(name) && self.requested.len() >= MAX_REQUESTS {
            return false;
        }
        self.requested.insert(name.to_owned(), enabled);
        self.refresh();
        true
    }

    /// The rules to apply: the configuration's, with live requests folded in.
    pub(crate) fn rules(&self) -> Vec<DeviceRule> {
        let mut rules = self.configured.clone();
        for (name, enabled) in &self.requested {
            match rules.iter_mut().find(|rule| rule.name == *name) {
                Some(rule) => rule.enabled = Some(*enabled),
                None => rules.push(DeviceRule { name: name.clone(), enabled: Some(*enabled), ..DeviceRule::default() }),
            }
        }
        rules
    }

    fn is_disabled(&self, name: &str) -> bool {
        self.disabled.iter().any(|disabled| disabled == name)
    }

    /// The names switched off since the last call.
    fn take_newly_disabled(&mut self) -> Vec<String> {
        let newly = self.disabled.iter().filter(|name| !self.released.contains(name)).cloned().collect();
        self.released.clone_from(&self.disabled);
        newly
    }

    fn refresh(&mut self) {
        self.disabled = self.rules().into_iter().filter(|rule| rule.enabled == Some(false)).map(|rule| rule.name).collect();
    }
}

fn enabled_in(rules: &[DeviceRule], name: &str) -> Option<bool> {
    rules.iter().find(|rule| rule.name == name).and_then(|rule| rule.enabled)
}

/// `hl.device({ name, enabled })`. The request is applied to every device
/// of that name before this returns, so the `ok` written after it is true.
/// Refused for a name no pointer, touch or tablet device on this desk
/// carries, and for switching off a name that any device with keys carries.
pub(crate) fn set_enabled(comp: &mut Compositor, name: &str, enabled: bool) -> bool {
    let records = &comp.wm.backend().input_devices;
    let pointing = records.iter().any(|record| record.name == name && (record.pointer || record.touch || record.tablet));
    let keys = records.iter().any(|record| record.name == name && record.keyboard);
    if !pointing || (keys && !enabled) {
        tracing::warn!(device = ?name, enabled, pointing, keys, "hl.device request refused");
        return false;
    }
    if !comp.wm.backend_mut().input_device_states.request(name, enabled) {
        tracing::warn!(device = ?name, limit = MAX_REQUESTS, "hl.device request refused: too many devices are already switched by name");
        return false;
    }
    let config = comp.wm.backend().pointer_config.clone();
    comp.apply_pointer_config(config, std::time::Instant::now());
    tracing::info!(device = ?name, enabled, "input device switched by request");
    true
}

/// Whether `event` came from a device that is switched off. Free while no
/// device is; after that, one name comparison per event.
pub(super) fn from_disabled_device<I: InputBackend>(comp: &Compositor, event: &InputEvent<I>) -> bool {
    let states = &comp.wm.backend().input_device_states;
    if states.disabled.is_empty() {
        return false;
    }
    event_device(event)
        .is_some_and(|device| !device.has_capability(DeviceCapability::Keyboard) && states.is_disabled(&device.name()))
}

/// The device an event came from. Lifecycle events are never dropped: a
/// switched-off device is still recorded and configured as it appears.
fn event_device<I: InputBackend>(event: &InputEvent<I>) -> Option<I::Device> {
    Some(match event {
        InputEvent::DeviceAdded { .. } | InputEvent::DeviceRemoved { .. } | InputEvent::Special(_) => return None,
        InputEvent::Keyboard { event } => event.device(),
        InputEvent::PointerMotion { event } => event.device(),
        InputEvent::PointerMotionAbsolute { event } => event.device(),
        InputEvent::PointerButton { event } => event.device(),
        InputEvent::PointerAxis { event } => event.device(),
        InputEvent::GestureSwipeBegin { event } => event.device(),
        InputEvent::GestureSwipeUpdate { event } => event.device(),
        InputEvent::GestureSwipeEnd { event } => event.device(),
        InputEvent::GesturePinchBegin { event } => event.device(),
        InputEvent::GesturePinchUpdate { event } => event.device(),
        InputEvent::GesturePinchEnd { event } => event.device(),
        InputEvent::GestureHoldBegin { event } => event.device(),
        InputEvent::GestureHoldEnd { event } => event.device(),
        InputEvent::TouchDown { event } => event.device(),
        InputEvent::TouchMotion { event } => event.device(),
        InputEvent::TouchUp { event } => event.device(),
        InputEvent::TouchCancel { event } => event.device(),
        InputEvent::TouchFrame { event } => event.device(),
        InputEvent::TabletToolAxis { event } => event.device(),
        InputEvent::TabletToolProximity { event } => event.device(),
        InputEvent::TabletToolTip { event } => event.device(),
        InputEvent::TabletToolButton { event } => event.device(),
        InputEvent::SwitchToggle { event } => event.device(),
    })
}

/// What each device holds at the seat, by device name.
#[derive(Debug, Default)]
pub(super) struct Holds {
    buttons: Vec<(String, u32)>,
    touches: Vec<(String, TouchSlot)>,
    swipe: Option<String>,
}

/// What one device was holding.
#[derive(Debug, Default, PartialEq)]
struct Held {
    buttons: Vec<u32>,
    touched: bool,
    swiping: bool,
}

impl Holds {
    fn button(&mut self, device: String, code: u32, pressed: bool) {
        self.buttons.retain(|(held, held_code)| !(*held == device && *held_code == code));
        if pressed {
            if self.buttons.len() + self.touches.len() >= MAX_HOLDS {
                self.buttons.remove(0);
            }
            self.buttons.push((device, code));
        }
    }

    fn touch(&mut self, device: String, slot: TouchSlot, down: bool) {
        self.touches.retain(|(held, held_slot)| !(*held == device && *held_slot == slot));
        if down {
            if self.touches.len() + self.buttons.len() >= MAX_HOLDS && !self.touches.is_empty() {
                self.touches.remove(0);
            }
            self.touches.push((device, slot));
        }
    }

    fn take(&mut self, device: &str) -> Held {
        let buttons = self.buttons.iter().filter(|(held, _)| held == device).map(|(_, code)| *code).collect();
        let touched = self.touches.iter().any(|(held, _)| held == device);
        let swiping = self.swipe.as_deref() == Some(device);
        self.forget(device);
        Held { buttons, touched, swiping }
    }

    fn forget(&mut self, device: &str) {
        self.buttons.retain(|(held, _)| held != device);
        self.touches.retain(|(held, _)| held != device);
        if self.swipe.as_deref() == Some(device) {
            self.swipe = None;
        }
    }
}

/// Keeps the ledger of holds. Only presses, releases and touch edges name
/// their device, so steady motion allocates nothing here.
pub(super) fn note_holds<I: InputBackend>(comp: &Compositor, event: &InputEvent<I>) {
    let seat = &comp.seat;
    match event {
        InputEvent::PointerButton { event } => {
            let (device, code, pressed) = (event.device().name(), event.button_code(), event.state() == ButtonState::Pressed);
            super::with_input(seat, |input| input.device_holds.button(device, code, pressed));
        }
        InputEvent::TouchDown { event } => {
            let (device, slot) = (event.device().name(), event.slot());
            super::with_input(seat, |input| input.device_holds.touch(device, slot, true));
        }
        InputEvent::TouchUp { event } => {
            let (device, slot) = (event.device().name(), event.slot());
            super::with_input(seat, |input| input.device_holds.touch(device, slot, false));
        }
        // A cancel ends every touch at the seat, whichever device sent it.
        InputEvent::TouchCancel { .. } => super::with_input(seat, |input| input.device_holds.touches.clear()),
        InputEvent::GestureSwipeBegin { event } => {
            let device = event.device().name();
            super::with_input(seat, |input| input.device_holds.swipe = Some(device));
        }
        InputEvent::GestureSwipeEnd { .. } => super::with_input(seat, |input| input.device_holds.swipe = None),
        InputEvent::DeviceRemoved { device } => {
            let device = device.name();
            super::with_input(seat, |input| input.device_holds.forget(&device));
        }
        _ => {}
    }
}

/// Releases whatever each newly switched-off device was holding: a button
/// through the same route a physical release takes, so an implicit grab,
/// a window drag and the client under it all see it end; a touch by
/// cancelling the sequence; a swipe by cancelling the gesture.
pub(crate) fn release_newly_disabled(comp: &mut Compositor) {
    let newly = comp.wm.backend_mut().input_device_states.take_newly_disabled();
    for device in newly {
        let seat = comp.seat.clone();
        let held = super::with_input(&seat, |input| input.device_holds.take(&device));
        if held == Held::default() {
            continue;
        }
        tracing::info!(
            device = ?device,
            buttons = held.buttons.len(),
            touched = held.touched,
            swiping = held.swiping,
            "releasing what a switched-off input device held"
        );
        if held.swiping {
            super::gestures::cancel(comp);
        }
        if held.touched {
            super::cancel_active_touches(comp);
        }
        let time = comp.start_time.elapsed().as_millis() as u32;
        for code in held.buttons {
            super::pointer_button(comp, time, code, ButtonState::Released, mouse_button(code).and_then(super::wm_button));
        }
    }
}

/// The `BTN_*` codes the window manager has a name for.
fn mouse_button(code: u32) -> Option<InputMouseButton> {
    match code {
        0x110 => Some(InputMouseButton::Left),
        0x111 => Some(InputMouseButton::Right),
        0x112 => Some(InputMouseButton::Middle),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(name: &str, enabled: Option<bool>) -> DeviceRule {
        DeviceRule { name: name.into(), enabled, ..DeviceRule::default() }
    }

    const TOUCHPAD: &str = "SynPS/2 Synaptics TouchPad";

    #[test]
    fn a_live_request_outlasts_unrelated_reloads_and_yields_to_the_configurations_newer_word() {
        let mut states = DeviceStates::default();
        states.configure(&[rule("Logitech MX Master 3", None)]);
        assert!(states.request(TOUCHPAD, false));
        assert!(states.is_disabled(TOUCHPAD));
        // An edit to anything else re-applies the same rules.
        states.configure(&[rule("Logitech MX Master 3", None)]);
        assert!(states.is_disabled(TOUCHPAD), "an unrelated reload must not undo a toggle");
        // Omarchy's data file arrives: the configuration agrees.
        states.configure(&[rule(TOUCHPAD, Some(false))]);
        assert!(states.is_disabled(TOUCHPAD));
        // The toggle turns it back on and deletes the file; the request is
        // newest until the next read without the file.
        assert!(states.request(TOUCHPAD, true));
        assert!(!states.is_disabled(TOUCHPAD));
        states.configure(&[rule(TOUCHPAD, Some(false))]);
        assert!(!states.is_disabled(TOUCHPAD), "the same configured word is not a newer one");
        states.configure(&[]);
        assert!(!states.is_disabled(TOUCHPAD));
        // A user edit that disables it after a live enable wins.
        states.configure(&[rule(TOUCHPAD, Some(false))]);
        assert!(states.is_disabled(TOUCHPAD), "the configuration changed its word, so it is newer");
        assert_eq!(states.rules(), [rule(TOUCHPAD, Some(false))]);
    }

    #[test]
    fn requests_fold_into_the_configured_rules_and_are_bounded() {
        let mut states = DeviceStates::default();
        let mut trackball = rule("Kensington Expert Mouse", None);
        trackball.sensitivity = Some(-0.5);
        states.configure(std::slice::from_ref(&trackball));
        assert!(states.request("Kensington Expert Mouse", false));
        assert_eq!(states.rules(), [DeviceRule { enabled: Some(false), ..trackball }]);
        for index in 1..MAX_REQUESTS {
            assert!(states.request(&format!("device {index}"), false));
        }
        assert!(!states.request("one device too many", false));
        assert!(states.request("device 1", true), "a remembered device can always change its answer");
        assert_eq!(states.disabled.len(), MAX_REQUESTS - 1);
    }

    #[test]
    fn a_device_is_released_once_per_switch_off() {
        let mut states = DeviceStates::default();
        states.configure(&[rule(TOUCHPAD, Some(false))]);
        assert_eq!(states.take_newly_disabled(), [TOUCHPAD]);
        states.configure(&[rule(TOUCHPAD, Some(false))]);
        assert!(states.take_newly_disabled().is_empty());
        states.configure(&[]);
        assert!(states.take_newly_disabled().is_empty());
        assert!(states.request(TOUCHPAD, false));
        assert_eq!(states.take_newly_disabled(), [TOUCHPAD], "switched off again, released again");
    }

    #[test]
    fn holds_are_taken_per_device_and_bounded() {
        let slot = |id: u32| TouchSlot::from(Some(id));
        let mut holds = Holds::default();
        holds.button("trackpad".into(), 0x110, true);
        holds.button("mouse".into(), 0x111, true);
        holds.button("trackpad".into(), 0x112, true);
        holds.button("trackpad".into(), 0x112, false);
        holds.touch("screen".into(), slot(0), true);
        holds.swipe = Some("trackpad".into());
        assert_eq!(holds.take("trackpad"), Held { buttons: vec![0x110], touched: false, swiping: true });
        assert_eq!(holds.take("trackpad"), Held::default(), "taking releases once");
        assert_eq!(holds.take("screen"), Held { buttons: vec![], touched: true, swiping: false });
        assert_eq!(holds.take("mouse").buttons, [0x111]);
        for code in 0..(MAX_HOLDS as u32 + 10) {
            holds.button("stuck".into(), code, true);
        }
        assert_eq!(holds.buttons.len(), MAX_HOLDS);
    }
}
