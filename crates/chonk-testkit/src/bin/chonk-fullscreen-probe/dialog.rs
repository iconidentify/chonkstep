//! A real transient xdg toplevel for fullscreen-family migration checks.
use super::*;

pub(super) struct Events;

pub(super) struct Dialog {
    role: XdgToplevel,
    xdg: XdgSurface,
    surface: WlSurface,
    buffer: Option<WlBuffer>,
    size: (i32, i32),
}

impl Drop for Dialog {
    fn drop(&mut self) {
        self.role.destroy();
        self.xdg.destroy();
        self.surface.destroy();
        if let Some(buffer) = self.buffer.take() {
            buffer.destroy();
        }
    }
}

pub(super) fn toggle(probe: &mut Probe, qh: &QueueHandle<Probe>) {
    if probe.dialog.take().is_some() {
        return;
    }
    let surface = probe.compositor.as_ref().unwrap().create_surface(qh, ());
    let xdg = probe
        .wm_base
        .as_ref()
        .unwrap()
        .get_xdg_surface(&surface, qh, Events);
    let role = xdg.get_toplevel(qh, Events);
    role.set_title("Space Dialog".into());
    role.set_app_id("space-dialog".into());
    role.set_parent(probe.toplevel.as_ref());
    surface.commit();
    probe.dialog = Some(Dialog {
        role,
        xdg,
        surface,
        buffer: None,
        size: (300, 180),
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
            let Some(dialog) = probe.dialog.as_mut().filter(|d| d.xdg == *xdg) else {
                return;
            };
            let (w, h) = dialog.size;
            let file = frame_file(w, h);
            let pool = probe
                .shm
                .as_ref()
                .unwrap()
                .create_pool(file.as_fd(), w * h * 4, qh, ());
            let buffer = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Argb8888, qh, ());
            pool.destroy();
            dialog.surface.attach(Some(&buffer), 0, 0);
            dialog.surface.damage(0, 0, w, h);
            dialog.surface.commit();
            if let Some(previous) = dialog.buffer.replace(buffer) {
                previous.destroy();
            }
            say("dialog painted");
        }
    }
}

impl Dispatch<XdgToplevel, Events> for Probe {
    fn event(
        probe: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        _: &Events,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Configure { width, height, .. } => {
                if let Some(dialog) = &mut probe.dialog {
                    dialog.size = (
                        if width > 0 { width } else { 300 },
                        if height > 0 { height } else { 180 },
                    );
                }
            }
            xdg_toplevel::Event::Close => probe.dialog = None,
            _ => {}
        }
    }
}
