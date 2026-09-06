//! Optional client-side constraint requests. F1 requests, F2 commits a cursor
//! hint, F3 destroys, F4 replaces the region. Input and fence events are only
//! observed; the compositor decides activation, positioning and delivery.

use wayland_client::protocol::{
    wl_callback, wl_compositor::WlCompositor, wl_pointer::WlPointer, wl_registry::WlRegistry,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::wp::pointer_constraints::zv1::client::{
    zwp_confined_pointer_v1::{self, ZwpConfinedPointerV1},
    zwp_locked_pointer_v1::{self, ZwpLockedPointerV1},
    zwp_pointer_constraints_v1::{Lifetime, ZwpPointerConstraintsV1},
};
use wayland_protocols::wp::relative_pointer::zv1::client::{
    zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1,
    zwp_relative_pointer_v1::{self, ZwpRelativePointerV1},
};

use super::{say, Probe};

#[derive(Clone, Copy)]
enum Kind {
    LockRegion,
    ConfineRegion,
    ConfineSurface,
    LockSurface,
}

#[derive(Default)]
pub(super) struct State {
    kind: Option<Kind>,
    manager: Option<ZwpPointerConstraintsV1>,
    relative_manager: Option<ZwpRelativePointerManagerV1>,
    relative: Option<ZwpRelativePointerV1>,
    locked: Option<ZwpLockedPointerV1>,
    confined: Option<ZwpConfinedPointerV1>,
}

impl State {
    pub fn from_args() -> Self {
        let kind = std::env::args().find_map(|arg| match arg.as_str() {
            "lock-region" => Some(Kind::LockRegion),
            "confine-region" => Some(Kind::ConfineRegion),
            "confine-full" => Some(Kind::ConfineSurface),
            "lock-full" => Some(Kind::LockSurface),
            _ => None,
        });
        Self {
            kind,
            ..Default::default()
        }
    }

    pub fn enabled(&self) -> bool {
        self.kind.is_some()
    }

    pub fn bind(
        &mut self,
        registry: &WlRegistry,
        name: u32,
        interface: &str,
        qh: &QueueHandle<Probe>,
    ) {
        if !self.enabled() {
            return;
        }
        match interface {
            "zwp_pointer_constraints_v1" => self.manager = Some(registry.bind(name, 1, qh, ())),
            "zwp_relative_pointer_manager_v1" => {
                self.relative_manager = Some(registry.bind(name, 1, qh, ()))
            }
            _ => unreachable!(),
        }
    }

    pub fn key(
        &mut self,
        key: u32,
        compositor: &WlCompositor,
        pointer: &WlPointer,
        surface: &WlSurface,
        qh: &QueueHandle<Probe>,
    ) -> bool {
        match key {
            59 => {
                assert!(
                    self.locked.is_none() && self.confined.is_none(),
                    "one constraint per surface"
                );
                if self.relative.is_none() {
                    self.relative = Some(
                        self.relative_manager
                            .as_ref()
                            .expect("relative pointer")
                            .get_relative_pointer(pointer, qh, ()),
                    );
                }
                let region = compositor.create_region(qh, ());
                region.add(100, 80, 120, 100);
                let manager = self.manager.as_ref().expect("pointer constraints");
                match self.kind.expect("enabled") {
                    Kind::LockRegion | Kind::LockSurface => {
                        let region = matches!(self.kind, Some(Kind::LockRegion)).then_some(&region);
                        self.locked = Some(manager.lock_pointer(
                            surface,
                            pointer,
                            region,
                            Lifetime::Persistent,
                            qh,
                            (),
                        ));
                    }
                    Kind::ConfineRegion | Kind::ConfineSurface => {
                        let region =
                            matches!(self.kind, Some(Kind::ConfineRegion)).then_some(&region);
                        self.confined = Some(manager.confine_pointer(
                            surface,
                            pointer,
                            region,
                            Lifetime::Persistent,
                            qh,
                            (),
                        ));
                    }
                }
                region.destroy();
                surface.commit();
            }
            60 => {
                self.locked
                    .as_ref()
                    .expect("a lock for the hint")
                    .set_cursor_position_hint(75.0, 65.0);
                surface.commit();
            }
            61 => {
                if let Some(locked) = self.locked.take() {
                    locked.destroy();
                }
                if let Some(confined) = self.confined.take() {
                    confined.destroy();
                }
            }
            62 => {
                let region = compositor.create_region(qh, ());
                region.add(250, 200, 80, 60);
                if let Some(locked) = &self.locked {
                    locked.set_region(Some(&region));
                }
                if let Some(confined) = &self.confined {
                    confined.set_region(Some(&region));
                }
                region.destroy();
                surface.commit();
            }
            _ => return false,
        }
        true
    }
}

impl Dispatch<wl_callback::WlCallback, u32> for Probe {
    fn event(
        _: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        key: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        say(&format!("constraint fence {key}"));
    }
}

impl Dispatch<ZwpLockedPointerV1, ()> for Probe {
    fn event(
        _: &mut Self,
        _: &ZwpLockedPointerV1,
        event: zwp_locked_pointer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_locked_pointer_v1::Event::Locked => say("constraint locked"),
            zwp_locked_pointer_v1::Event::Unlocked => say("constraint unlocked"),
            _ => {}
        }
    }
}

impl Dispatch<ZwpConfinedPointerV1, ()> for Probe {
    fn event(
        _: &mut Self,
        _: &ZwpConfinedPointerV1,
        event: zwp_confined_pointer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_confined_pointer_v1::Event::Confined => say("constraint confined"),
            zwp_confined_pointer_v1::Event::Unconfined => say("constraint unconfined"),
            _ => {}
        }
    }
}

impl Dispatch<ZwpRelativePointerV1, ()> for Probe {
    fn event(
        probe: &mut Self,
        _: &ZwpRelativePointerV1,
        event: zwp_relative_pointer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_relative_pointer_v1::Event::RelativeMotion {
            dx,
            dy,
            dx_unaccel,
            dy_unaccel,
            ..
        } = event
        {
            probe.report_at("relative", (dx, dy));
            say(&format!("relative raw {dx_unaccel:.4} {dy_unaccel:.4}"));
        }
    }
}
wayland_client::delegate_noop!(Probe: ignore ZwpPointerConstraintsV1);
wayland_client::delegate_noop!(Probe: ignore ZwpRelativePointerManagerV1);
wayland_client::delegate_noop!(Probe: ignore wayland_client::protocol::wl_region::WlRegion);
