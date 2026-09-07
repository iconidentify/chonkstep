//! A colored native application menu for pixel/input tests in every layout.
use super::*;
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_protocols::xdg::shell::client::{xdg_popup, xdg_positioner};

pub(super) struct Events;

pub(super) struct Popup {
    role: xdg_popup::XdgPopup,
    xdg: XdgSurface,
    surface: WlSurface,
    buffer: Option<WlBuffer>,
    pointer: WlPointer,
    entered: bool,
}

impl Drop for Popup {
    fn drop(&mut self) {
        self.role.destroy();
        self.xdg.destroy();
        self.surface.destroy();
        self.pointer.release();
        if let Some(buffer) = self.buffer.take() {
            buffer.destroy();
        }
    }
}

pub(super) fn toggle(probe: &mut Probe, qh: &QueueHandle<Probe>) {
    if probe.popup.take().is_some() {
        say("popup closed");
        return;
    }
    let base = probe.wm_base.as_ref().unwrap();
    let positioner = base.create_positioner(qh, ());
    positioner.set_size(120, 80);
    positioner.set_anchor_rect(50, 50, 1, 1);
    positioner.set_anchor(xdg_positioner::Anchor::TopLeft);
    positioner.set_gravity(xdg_positioner::Gravity::BottomRight);
    let surface = probe.compositor.as_ref().unwrap().create_surface(qh, ());
    let xdg = base.get_xdg_surface(&surface, qh, Events);
    let role = xdg.get_popup(probe.xdg_surface.as_ref(), &positioner, qh, Events);
    positioner.destroy();
    let pointer = probe.seat.as_ref().unwrap().get_pointer(qh, Events);
    surface.commit();
    probe.popup = Some(Popup {
        role,
        xdg,
        surface,
        buffer: None,
        pointer,
        entered: false,
    });
}

impl Dispatch<XdgSurface, Events> for Probe {
    fn event(
        probe: &mut Self,
        xdg: &XdgSurface,
        event: xdg_surface::Event,
        _: &Events,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg.ack_configure(serial);
            let Some(popup) = probe.popup.as_mut().filter(|p| p.xdg == *xdg) else {
                return;
            };
            let mut file = tempfile::tempfile().unwrap();
            let pixels = [0xff19e63au32.to_ne_bytes(); 120 * 80];
            file.write_all(pixels.as_flattened()).unwrap();
            let pool = probe
                .shm
                .as_ref()
                .unwrap()
                .create_pool(file.as_fd(), 120 * 80 * 4, qh, ());
            let buffer = pool.create_buffer(0, 120, 80, 120 * 4, wl_shm::Format::Argb8888, qh, ());
            pool.destroy();
            popup.surface.attach(Some(&buffer), 0, 0);
            popup.surface.damage(0, 0, 120, 80);
            popup.surface.commit();
            if let Some(previous) = popup.buffer.replace(buffer) {
                previous.destroy();
            }
            say("popup painted");
        }
    }
}

impl Dispatch<xdg_popup::XdgPopup, Events> for Probe {
    fn event(
        probe: &mut Self,
        _: &xdg_popup::XdgPopup,
        event: xdg_popup::Event,
        _: &Events,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_popup::Event::PopupDone = event {
            probe.popup = None;
        }
    }
}

impl Dispatch<xdg_positioner::XdgPositioner, ()> for Probe {
    fn event(
        _: &mut Self,
        _: &xdg_positioner::XdgPositioner,
        _: xdg_positioner::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlPointer, Events> for Probe {
    fn event(
        probe: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        _: &Events,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(popup) = probe.popup.as_mut() else {
            return;
        };
        match event {
            wl_pointer::Event::Enter { surface, .. } => popup.entered = surface == popup.surface,
            wl_pointer::Event::Leave { .. } => popup.entered = false,
            wl_pointer::Event::Button {
                state: WEnum::Value(wl_pointer::ButtonState::Pressed),
                ..
            } if popup.entered => say("popup clicked"),
            _ => {}
        }
    }
}
