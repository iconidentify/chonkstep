//! Seat focus must retain the X11 window, not only its Wayland backing surface.
//! Smithay's X11 keyboard target performs ICCCM SetInputFocus/WM_TAKE_FOCUS and
//! balances it on leave. Using the bare wl_surface skips that entire contract:
//! a window can look active while its X server still sends keys elsewhere.

use std::borrow::Cow;

use smithay::backend::input::KeyState;
use smithay::input::keyboard::{KeyboardTarget, KeysymHandle, ModifiersState};
use smithay::input::Seat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{IsAlive, Serial};
use smithay::wayland::seat::WaylandFocus;
use smithay::xwayland::xwm::XwmId;
use smithay::xwayland::X11Surface;

use crate::state::{Compositor, ManagedSurface};

#[derive(Clone, Debug)]
/// Native surface plus the X11 identity needed for ICCCM focus, when present.
pub struct KeyboardFocus {
    surface: WlSurface,
    x11: Option<X11Surface>,
}

impl KeyboardFocus {
    /// Use the window record when it is already known. XWayland can associate
    /// its wl_surface before its first commit populates the reverse index;
    /// focus must not lose the X11 identity during that interval.
    pub(crate) fn from_managed(managed: &ManagedSurface) -> Option<Self> {
        Some(Self {
            surface: managed.wl_surface()?,
            x11: match managed {
                ManagedSurface::X11(window) => Some(window.clone()),
                ManagedSurface::Xdg(_) => None,
            },
        })
    }

    pub(crate) fn new(comp: &Compositor, surface: WlSurface) -> Self {
        let backend = comp.wm.backend();
        let x11 = backend
            .window_for_surface(&surface)
            .and_then(|window| backend.windows.get(&window))
            .and_then(|record| match &record.surface {
                ManagedSurface::X11(window) => Some(window.clone()),
                ManagedSurface::Xdg(_) => None,
            });
        Self { surface, x11 }
    }

    pub(crate) fn surface(&self) -> &WlSurface {
        &self.surface
    }

    /// Use the focus target's generation, even before the first buffer commit
    /// populates the backend's wl_surface reverse index.
    pub(crate) fn xwm_id(&self) -> Option<XwmId> {
        if !self.alive() {
            return None;
        }
        self.x11.as_ref()?.xwm_id()
    }
}

impl PartialEq for KeyboardFocus {
    fn eq(&self, other: &Self) -> bool {
        self.surface == other.surface && self.x11 == other.x11
    }
}

impl IsAlive for KeyboardFocus {
    fn alive(&self) -> bool {
        self.surface.alive() && self.x11.as_ref().is_none_or(IsAlive::alive)
    }
}

impl WaylandFocus for KeyboardFocus {
    fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        Some(Cow::Borrowed(&self.surface))
    }
}

impl KeyboardTarget<Compositor> for KeyboardFocus {
    fn enter(
        &self,
        seat: &Seat<Compositor>,
        comp: &mut Compositor,
        mut keys: Vec<KeysymHandle<'_>>,
        serial: Serial,
    ) {
        keys.retain(|key| comp.mac_keyboard.may_enter(key.raw_code()));
        match &self.x11 {
            Some(window) => window.enter(seat, comp, keys, serial),
            None => KeyboardTarget::enter(&self.surface, seat, comp, keys, serial),
        }
    }

    fn leave(&self, seat: &Seat<Compositor>, comp: &mut Compositor, serial: Serial) {
        comp.mac_keyboard.leave(self);
        match &self.x11 {
            Some(window) => window.leave(seat, comp, serial),
            None => KeyboardTarget::leave(&self.surface, seat, comp, serial),
        }
    }

    fn key(
        &self,
        seat: &Seat<Compositor>,
        comp: &mut Compositor,
        key: KeysymHandle<'_>,
        state: KeyState,
        serial: Serial,
        time: u32,
    ) {
        if comp.mac_keyboard.suppress_key { return; }
        // Smithay normally sends key then modifiers, which is correct for a
        // physical modifier key. A translated chord needs its projection in
        // place before the letter, otherwise clients execute the previous mask.
        if let Some(modifiers) = comp.mac_keyboard.modifiers {
            match &self.x11 {
                Some(window) => window.modifiers(seat, comp, modifiers, serial),
                None => self.surface.modifiers(seat, comp, modifiers, serial),
            }
        }
        match &self.x11 {
            Some(window) => window.key(seat, comp, key, state, serial, time),
            None => self.surface.key(seat, comp, key, state, serial, time),
        }
    }

    fn modifiers(
        &self,
        seat: &Seat<Compositor>,
        comp: &mut Compositor,
        modifiers: ModifiersState,
        serial: Serial,
    ) {
        let modifiers = comp.mac_keyboard.modifiers.unwrap_or(modifiers);
        match &self.x11 {
            Some(window) => window.modifiers(seat, comp, modifiers, serial),
            None => self.surface.modifiers(seat, comp, modifiers, serial),
        }
    }
}
