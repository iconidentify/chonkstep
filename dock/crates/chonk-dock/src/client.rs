//! Display selection and the shared native-client surface interface.
use crate::{
    surface::{Backend, DragHandle, Role},
    wayland::Wayland,
    x11::X11,
};
use std::{os::fd::AsRawFd, time::Duration};
use wm_theme_api::{DecorationBuffer, Point, PopupGrab, PopupHost, Rect};

#[derive(Clone, Debug)]
pub enum Event {
    Motion(u32, Point),
    Leave,
    Button(u32, Point, u32, bool),
    Scroll(u32, Point, i32),
    Escape,
    Dismiss(u32),
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Wayland,
    X11,
}

impl Platform {
    pub fn detect() -> Self {
        Self::select(
            std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
            std::env::var_os("WAYLAND_DISPLAY").is_some()
                || std::env::var_os("WAYLAND_SOCKET").is_some(),
        )
    }
    fn select(session: Option<&str>, wayland: bool) -> Self {
        if session != Some("x11") && wayland {
            Self::Wayland
        } else {
            Self::X11
        }
    }
    pub fn display_name(self) -> String {
        std::env::var(match self {
            Self::Wayland => "WAYLAND_DISPLAY",
            Self::X11 => "DISPLAY",
        })
        .unwrap_or_else(|_| "default".into())
    }
    pub fn socket_key(self) -> String {
        let name = chonk_ipc::sanitize_display(&self.display_name());
        match self {
            Self::Wayland => name,
            Self::X11 => format!("x11-{name}"),
        }
    }
}

pub enum Client {
    Wayland {
        connection: wayland_client::Connection,
        queue: wayland_client::EventQueue<Wayland>,
        state: Box<Wayland>,
    },
    X11(Box<X11>),
}

macro_rules! state {
    ($self:expr, $s:ident => $body:expr) => {
        match $self {
            Client::Wayland { state: $s, .. } => $body,
            Client::X11($s) => $body,
        }
    };
}

impl Client {
    pub fn connect(platform: Platform, output: Option<&str>) -> Result<Self> {
        Ok(match platform {
            Platform::Wayland => {
                let (connection, queue, state) = Wayland::connect(output)?;
                Self::Wayland {
                    connection,
                    queue,
                    state: Box::new(state),
                }
            }
            Platform::X11 => Self::X11(Box::new(X11::connect(output)?)),
        })
    }
    pub fn dispatch(&mut self) -> Result<()> {
        match self {
            Self::Wayland { queue, state, .. } => {
                queue.dispatch_pending(state)?;
            }
            Self::X11(state) => {
                state.dispatch()?;
            }
        }
        Ok(())
    }
    pub fn screen(&self) -> Rect {
        state!(self, s => s.screen())
    }
    pub fn scale(&self) -> f32 {
        match self {
            Self::Wayland { state, .. } => state.scale() as f32,
            Self::X11(state) => state.scale(),
        }
    }
    pub fn closed(&self) -> bool {
        state!(self, s => s.closed)
    }
    pub fn take_changed(&mut self) -> bool {
        state!(self, s => std::mem::take(&mut s.changed))
    }
    pub fn take_windows_changed(&mut self) -> bool {
        state!(self, s => std::mem::take(&mut s.windows_changed))
    }
    pub fn take_events(&mut self) -> Vec<Event> {
        state!(self, s => std::mem::take(&mut s.events))
    }
    pub fn running(&self) -> Vec<(String, u32)> {
        state!(self, s => s.running())
    }
    pub fn window_app_id(&self, id: u32) -> Option<&str> {
        state!(self, s => s.windows.get(&id).map(|w| w.app_id.as_str()))
    }
    pub fn minimized(&self) -> Vec<(u32, String)> {
        state!(self, s => s.windows.iter().filter(|(_, w)| w.ready && w.minimized)
            .map(|(id, w)| (*id, w.title.clone())).collect())
    }
    pub fn geometry(&self, id: u32) -> Option<Rect> {
        state!(self, s => s.geometry(id))
    }
    pub fn activate(&self, id: u32) {
        state!(self, s => s.activate(id))
    }
    pub fn is_backdrop(&self, id: u32) -> bool {
        state!(self, s => s.is_backdrop(id))
    }
    pub fn sync_backdrop(&mut self) {
        state!(self, s => s.sync_backdrop())
    }
    pub fn present(&mut self) -> Result<()> {
        state!(self, s => { s.present()?; Ok(()) })
    }
    pub fn workspace(&self) -> Option<(usize, usize)> {
        match self {
            Self::X11(s) => Some(s.workspace()),
            _ => None,
        }
    }
    pub fn focus_workspace(&self, index: usize) {
        if let Self::X11(s) = self {
            s.focus_workspace(index);
        }
    }
    pub fn wait(&mut self, mut descriptors: Vec<libc::pollfd>, timeout: Duration) -> Result<()> {
        match self {
            Self::Wayland {
                connection, queue, ..
            } => {
                let write = match connection.flush() {
                    Ok(()) => false,
                    Err(wayland_client::backend::WaylandError::Io(e))
                        if e.kind() == std::io::ErrorKind::WouldBlock =>
                    {
                        true
                    }
                    Err(e) => return Err(e.into()),
                };
                let Some(guard) = queue.prepare_read() else {
                    return Ok(());
                };
                descriptors.push(libc::pollfd {
                    fd: guard.connection_fd().as_raw_fd(),
                    events: libc::POLLIN | if write { libc::POLLOUT } else { 0 },
                    revents: 0,
                });
                poll(&mut descriptors, timeout)?;
                if descriptors.last().unwrap().revents
                    & (libc::POLLIN | libc::POLLHUP | libc::POLLERR)
                    != 0
                {
                    match guard.read() {
                        Ok(_) => {}
                        Err(wayland_client::backend::WaylandError::Io(e))
                            if e.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(e) => return Err(e.into()),
                    }
                }
            }
            Self::X11(state) => {
                // Round trips while painting or grabbing can queue input in x11rb.
                if state.dispatch()? {
                    return Ok(());
                }
                descriptors.push(libc::pollfd {
                    fd: state.fd(),
                    events: libc::POLLIN,
                    revents: 0,
                });
                poll(&mut descriptors, timeout)?;
            }
        }
        Ok(())
    }
}

fn poll(descriptors: &mut [libc::pollfd], timeout: Duration) -> std::io::Result<()> {
    // SAFETY: descriptors are initialized, live for the call, and the count
    // exactly matches this borrowed slice. poll retains no pointer.
    let result = unsafe {
        libc::poll(
            descriptors.as_mut_ptr(),
            descriptors.len() as libc::nfds_t,
            timeout.as_millis().clamp(1, i32::MAX as u128) as i32,
        )
    };
    if result < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

macro_rules! forward {
    ($trait:ident, $name:ident($($arg:ident: $ty:ty),*) $(-> $ret:ty)?) => {
        fn $name(&mut self, $($arg: $ty),*) $(-> $ret)? {
            state!(self, s => $trait::$name(s.as_mut(), $($arg),*))
        }
    };
}
impl Backend for Client {
    type ShellId = u32;
    type WindowId = u32;
    fn display_name(&self) -> String {
        match self {
            Self::Wayland { .. } => Platform::Wayland.socket_key(),
            Self::X11(_) => Platform::X11.socket_key(),
        }
    }
    forward!(Backend, create_shell_surface(rect: Rect, bg: (u8,u8,u8), input: bool) -> Option<u32>);
    forward!(Backend, set_role(id: u32, role: Role));
    forward!(Backend, map_shell_surface(id: u32));
    forward!(Backend, unmap_shell_surface(id: u32));
    forward!(Backend, configure_shell_surface(id: u32, rect: Rect));
    forward!(Backend, paint_shell_surface(id: u32, pixels: &DecorationBuffer));
    forward!(Backend, release_shell_buffer(id: u32));
    forward!(Backend, destroy_shell_surface(id: u32));
    forward!(Backend, raise_shell_surface(id: u32));
    forward!(Backend, grab_pointer_for_drag() -> DragHandle);
    forward!(Backend, ungrab_pointer(grab: DragHandle));
    forward!(Backend, panel_keyboard(enabled: bool));
}
impl PopupHost for Client {
    type PopupId = u32;
    forward!(PopupHost, create_popup(rect: Rect, bg: (u8,u8,u8)) -> Option<u32>);
    forward!(PopupHost, destroy_popup(id: u32));
    forward!(PopupHost, paint_popup(id: u32, pixels: &DecorationBuffer));
    forward!(PopupHost, grab_pointer() -> PopupGrab);
    forward!(PopupHost, ungrab_pointer(grab: PopupGrab));
    forward!(PopupHost, grab_keyboard());
    forward!(PopupHost, ungrab_keyboard());
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn x11_session_takes_precedence_over_stale_wayland_environment() {
        assert_eq!(Platform::select(Some("x11"), true), Platform::X11);
        assert_eq!(Platform::select(None, false), Platform::X11);
        assert_eq!(Platform::select(Some("wayland"), true), Platform::Wayland);
        assert_eq!(Platform::select(None, true), Platform::Wayland);
    }
}
