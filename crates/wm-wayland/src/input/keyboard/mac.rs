//! Mac client delivery preserves Smithay's physical XKB state and keymap
//! restoration, while projecting only the modifiers/key of a translated chord.

use smithay::backend::input::KeyState;
use smithay::input::keyboard::{KeyboardHandle, Keycode, ModifiersState};
use smithay::utils::Serial;
use wm_core::{KeyCombo, Modifiers};

use super::KeyboardFocus;
use crate::state::Compositor;

const MAX_KEYS: usize = 776;

#[derive(Clone)]
struct Held {
    code: Keycode,
    cancelled: bool,
    removed: Modifiers,
    added: Modifiers,
    focus: Option<KeyboardFocus>,
}

pub(crate) struct MacKeyboard {
    held: Vec<Option<Held>>,
    pub(crate) modifiers: Option<ModifiersState>,
    pub(crate) suppress_key: bool,
}

impl Default for MacKeyboard {
    fn default() -> Self {
        Self {
            held: vec![None; MAX_KEYS],
            modifiers: None,
            suppress_key: false,
        }
    }
}

impl MacKeyboard {
    pub(crate) fn leave(&mut self, focus: &KeyboardFocus) {
        for held in self.held.iter_mut().flatten() {
            if held.focus.as_ref() == Some(focus) {
                held.focus = None;
            }
        }
    }

    pub(crate) fn may_enter(&self, code: Keycode) -> bool {
        !self
            .held
            .iter()
            .flatten()
            .any(|held| !held.cancelled && held.code == code)
    }

    pub(crate) fn has_held(&self, code: Keycode) -> bool {
        self.held.get(u32::from(code) as usize).is_some_and(Option::is_some)
    }

    /// Balance translated deliveries before Smithay releases physical keys on
    /// resume. Return physical codes whose releases must then be swallowed.
    pub(crate) fn drain(&mut self) -> Vec<(Keycode, Option<Keycode>, Option<KeyboardFocus>)> {
        self.held
            .iter_mut()
            .enumerate()
            .filter_map(|(code, held)| {
                held.take()
                    .map(|held| ((code as u32).into(), (!held.cancelled).then_some(held.code), held.focus))
            })
            .collect()
    }
}

/// Forward after the physical event has passed input_intercept. No synthetic
/// event re-enters global shortcut matching or changes physical Control state.
pub(crate) struct Delivery {
    pub code: Keycode,
    pub state: KeyState,
    pub serial: Serial,
    pub time: u32,
    pub combo: KeyCombo,
    pub physical: ModifiersState,
    pub eligible: bool,
}

pub(crate) fn forward(comp: &mut Compositor, keyboard: &KeyboardHandle<Compositor>, event: Delivery) {
    let Delivery {
        code,
        state,
        serial,
        time,
        combo,
        physical,
        eligible,
    } = event;
    let index = u32::from(code) as usize;
    let focus = keyboard.current_focus();
    if state == KeyState::Pressed && comp.mac_keyboard.has_held(code) {
        return;
    }
    let held = if state == KeyState::Released {
        comp.mac_keyboard.held.get_mut(index).and_then(Option::take)
    } else {
        None
    };
    let mut out_code = code;
    let mut desired = combo.modifiers;
    let mut translated = false;
    let mut suppress = false;
    if let Some(held) = held {
        if held.cancelled {
            return;
        }
        out_code = held.code;
        suppress = held.focus.is_none() || held.focus != focus;
        if physical.logo || physical.alt {
            desired = (combo.modifiers & !held.removed) | held.added;
        }
        translated = true;
    } else if state == KeyState::Pressed
        && eligible
        && comp.wm.mac_mode()
        && index < MAX_KEYS
        && combo.modifiers.intersects(Modifiers::SUPER | Modifiers::ALT)
        && !physical.iso_level3_shift
        && !physical.iso_level5_shift
    {
        if let Some(target) = focus.as_ref() {
            let backend = comp.wm.backend();
            let identity = backend
                .window_for_surface(target.surface())
                .and_then(|id| backend.windows.get(&id))
                .and_then(|record| record.app_id.as_deref())
                .unwrap_or("");
            let profile = comp.wm.interaction_config().profile(identity);
            if let Some(replacement) = wm_core::interaction::mac_chord(profile, combo) {
                let replacement_code = if replacement.keysym == combo.keysym {
                    Some(code)
                } else {
                    // Standard XKB key names identify navigation keys on every
                    // layout; no evdev-number assumption or keymap swap.
                    let name = match replacement.keysym {
                        0xff50 => "HOME",
                        0xff57 => "END",
                        0xff1b => "ESC",
                        0xff51 => "LEFT",
                        0xff53 => "RGHT",
                        0xff55 => "PGUP",
                        0xff56 => "PGDN",
                        0xff0d => "RTRN",
                        0xffff => "DELE",
                        _ => "",
                    };
                    keyboard.with_xkb_state(comp, |context| {
                        let xkb = context.xkb().lock().unwrap();
                        // SAFETY: no borrowed keymap or ref-count leaves this lock.
                        let keymap = unsafe { xkb.keymap() };
                        if !name.is_empty() {
                            return keymap.key_by_name(name);
                        }
                        // A printable replacement follows the active layout;
                        // never assume a US physical position for that letter.
                        (u32::from(keymap.min_keycode())..=u32::from(keymap.max_keycode()))
                            .map(Keycode::from)
                            .find(|key| {
                                keymap
                                    .key_get_syms_by_level(*key, physical.serialized.layout_effective, 0)
                                    .iter()
                                    .any(|sym| sym.raw() == replacement.keysym)
                            })
                    })
                };
                if let Some(replacement_code) = replacement_code {
                    out_code = replacement_code;
                    desired = replacement.modifiers;
                    translated = true;
                    if state == KeyState::Pressed {
                        let cancelled = keyboard.forwarded_key_is_pressed(out_code);
                        comp.mac_keyboard.held[index] = Some(Held {
                            code: out_code,
                            cancelled,
                            removed: combo.modifiers & !desired,
                            added: desired & !combo.modifiers,
                            focus: focus.clone(),
                        });
                        if cancelled {
                            return;
                        }
                    }
                }
            }
        }
    }
    // A physical navigation key can collide with a translated navigation key
    // already down. Ignore the extra press and its release as one pair, keeping
    // the original owner balanced in Smithay's forwarded-key ledger.
    if !translated && state == KeyState::Pressed && index < MAX_KEYS && keyboard.forwarded_key_is_pressed(code) {
        comp.mac_keyboard.held[index] = Some(Held {
            code,
            cancelled: true,
            removed: Modifiers::empty(),
            added: Modifiers::empty(),
            focus,
        });
        return;
    }
    if suppress {
        // The old client balanced its keys on leave. Sending this release
        // through an IME can asynchronously reinject it into the new focus,
        // bypassing KeyboardFocus's synchronous suppression flag.
        keyboard.retire_forwarded_key(out_code);
        return;
    }
    let modifiers = if translated {
        keyboard.with_xkb_state(comp, |context| {
            let xkb = context.xkb().lock().unwrap();
            // SAFETY: all keymap operations finish before releasing the guard.
            let keymap = unsafe { xkb.keymap() };
            let mut mask = 0;
            let mut selected = 0;
            for (name, flag) in [
                ("Control", Modifiers::CONTROL),
                ("Mod1", Modifiers::ALT),
                ("Mod4", Modifiers::SUPER),
                ("Shift", Modifiers::SHIFT),
            ] {
                let index = keymap.mod_get_index(name);
                if index < 32 {
                    mask |= 1u32 << index;
                    if desired.contains(flag) {
                        selected |= 1u32 << index;
                    }
                }
            }
            let mut out = physical;
            out.ctrl = desired.contains(Modifiers::CONTROL);
            out.alt = desired.contains(Modifiers::ALT);
            out.logo = desired.contains(Modifiers::SUPER);
            out.shift = desired.contains(Modifiers::SHIFT);
            out.serialized.depressed = (out.serialized.depressed & !mask) | selected;
            out.serialized.latched &= !mask;
            out.serialized.locked &= !mask;
            out
        })
    } else {
        physical
    };
    comp.mac_keyboard.modifiers = Some(modifiers);
    comp.mac_keyboard.suppress_key = suppress;
    // Always announce the current projection. This restores modifiers even if
    // physical XKB reports no change after a translated press or keymap handoff.
    keyboard.input_forward_with_modifiers(comp, out_code, state, serial, time, modifiers);
    comp.mac_keyboard.modifiers = None;
    comp.mac_keyboard.suppress_key = false;
    if state == KeyState::Pressed && !suppress && eligible && comp.wm.mac_mode()
        && combo.modifiers == Modifiers::SUPER && matches!(combo.keysym, 0x63 | 0x78) {
        if let Some(focus) = focus { comp.mac_copy_order.copied(focus); }
    }
}
