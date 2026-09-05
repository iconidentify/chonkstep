//! `wlr-virtual-pointer-unstable-v1`: pointer injection for assistive
//! devices, automation, and the input half of remote-control stacks.
//!
//! Requests are accumulated until the protocol's `frame`, then lowered
//! through `crate::input`'s physical-pointer helpers. That shared path
//! is intentional: virtual motion obeys confinement and active grabs,
//! clicks drive shell and wm-core hit-testing, scroll reaches dock
//! widgets, and every event counts as idle activity. A synthetic
//! pointer is user activity when it represents an eye tracker or switch
//! device; a runaway client can therefore keep the session awake, just
//! as a virtual keyboard can.

use std::sync::Mutex;

use smithay::backend::input::AxisSource as BackendAxisSource;
use smithay::output::Output;
use smithay::reexports::wayland_protocols_wlr::virtual_pointer::v1::server::zwlr_virtual_pointer_manager_v1::{
    self, ZwlrVirtualPointerManagerV1,
};
use smithay::reexports::wayland_protocols_wlr::virtual_pointer::v1::server::zwlr_virtual_pointer_v1::{
    self, ZwlrVirtualPointerV1,
};
use smithay::reexports::wayland_server::protocol::{wl_output::WlOutput, wl_pointer};
use smithay::reexports::wayland_server::{
    backend::GlobalId, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};
use smithay::utils::{Logical, Point as LogicalPoint};

use crate::state::Compositor;

const VERSION: u32 = 2;

/// Held for the global's lifetime. The policy seam deliberately has
/// the same shape as virtual-keyboard's; every client on this socket is
/// currently a full user-session peer, and this is where a future
/// security-context restriction belongs.
pub(crate) struct VirtualPointerState {
    _global: GlobalId,
}

#[derive(Default)]
struct Pending {
    events: Vec<PointerEvent>,
    horizontal: Option<f64>,
    vertical: Option<f64>,
    horizontal_discrete: Option<i32>,
    vertical_discrete: Option<i32>,
    stop_horizontal: bool,
    stop_vertical: bool,
    axis_time: u32,
    axis_source: Option<BackendAxisSource>,
}

enum PointerEvent {
    Motion {
        time: u32,
        dx: f64,
        dy: f64,
    },
    MotionAbsolute {
        time: u32,
        x: u32,
        y: u32,
        x_extent: u32,
        y_extent: u32,
    },
    Button {
        time: u32,
        button: u32,
        pressed: bool,
    },
}

struct PointerData {
    output: Option<WlOutput>,
    pending: Mutex<Pending>,
}

pub(crate) fn init(display_handle: &DisplayHandle) -> VirtualPointerState {
    let global =
        display_handle.create_global::<Compositor, ZwlrVirtualPointerManagerV1, ()>(VERSION, ());
    tracing::info!(version = VERSION, "virtual-pointer advertised");
    VirtualPointerState { _global: global }
}

fn may_create_virtual_pointer(_client: &Client) -> bool {
    true
}

fn map_absolute_axis(value: u32, extent: u32, size: u32) -> f64 {
    if extent == 0 || size <= 1 {
        0.0
    } else {
        (f64::from(value.min(extent)) / f64::from(extent)) * f64::from(size - 1)
    }
}

fn absolute_position(
    comp: &Compositor,
    output: Option<&WlOutput>,
    x: u32,
    y: u32,
    x_extent: u32,
    y_extent: u32,
) -> LogicalPoint<f64, Logical> {
    let output = output.and_then(Output::from_resource);
    let (origin_x, origin_y, width, height) = output
        .as_ref()
        .and_then(|wanted| {
            comp.outputs
                .iter()
                .find(|entry| &entry.output == wanted)
                .map(|entry| {
                    (
                        entry.position.x,
                        entry.position.y,
                        entry.size.w,
                        entry.size.h,
                    )
                })
        })
        .unwrap_or((
            0,
            0,
            comp.wm.backend().output_size.w,
            comp.wm.backend().output_size.h,
        ));
    (
        f64::from(origin_x) + map_absolute_axis(x, x_extent, width),
        f64::from(origin_y) + map_absolute_axis(y, y_extent, height),
    )
        .into()
}

fn flush(comp: &mut Compositor, data: &PointerData) {
    let pending = std::mem::take(&mut *data.pending.lock().unwrap());
    for event in pending.events {
        match event {
            PointerEvent::Motion { time, dx, dy } => {
                crate::input::inject_pointer_motion(comp, dx, dy, time)
            }
            PointerEvent::MotionAbsolute {
                time,
                x,
                y,
                x_extent,
                y_extent,
            } => {
                let position =
                    absolute_position(comp, data.output.as_ref(), x, y, x_extent, y_extent);
                crate::input::inject_pointer_motion_absolute(comp, position, time);
            }
            PointerEvent::Button {
                time,
                button,
                pressed,
            } => crate::input::inject_pointer_button(comp, time, button, pressed),
        }
    }
    let has_axis = pending.horizontal.is_some()
        || pending.vertical.is_some()
        || pending.horizontal_discrete.is_some()
        || pending.vertical_discrete.is_some()
        || pending.stop_horizontal
        || pending.stop_vertical;
    if has_axis {
        crate::input::inject_pointer_axis(
            comp,
            pending.axis_time,
            pending.horizontal,
            pending.vertical,
            pending.horizontal_discrete,
            pending.vertical_discrete,
            pending.axis_source.unwrap_or(BackendAxisSource::Wheel),
            pending.stop_horizontal,
            pending.stop_vertical,
        );
    }
}

impl GlobalDispatch<ZwlrVirtualPointerManagerV1, ()> for Compositor {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrVirtualPointerManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, _global_data: &()) -> bool {
        may_create_virtual_pointer(&client)
    }
}

impl Dispatch<ZwlrVirtualPointerManagerV1, ()> for Compositor {
    fn request(
        _state: &mut Self,
        client: &Client,
        _resource: &ZwlrVirtualPointerManagerV1,
        request: zwlr_virtual_pointer_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_virtual_pointer_manager_v1::Request;
        match request {
            Request::CreateVirtualPointer { seat: _, id } => {
                if may_create_virtual_pointer(client) {
                    data_init.init(
                        id,
                        PointerData {
                            output: None,
                            pending: Mutex::new(Pending::default()),
                        },
                    );
                }
            }
            Request::CreateVirtualPointerWithOutput {
                seat: _,
                output,
                id,
            } => {
                if may_create_virtual_pointer(client) {
                    data_init.init(
                        id,
                        PointerData {
                            output,
                            pending: Mutex::new(Pending::default()),
                        },
                    );
                }
            }
            Request::Destroy => {}
            _ => {}
        }
    }
}

impl Dispatch<ZwlrVirtualPointerV1, PointerData> for Compositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZwlrVirtualPointerV1,
        request: zwlr_virtual_pointer_v1::Request,
        data: &PointerData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_virtual_pointer_v1::{Error, Request};
        let mut pending = data.pending.lock().unwrap();
        match request {
            Request::Motion { time, dx, dy } => {
                pending.events.push(PointerEvent::Motion { time, dx, dy })
            }
            Request::MotionAbsolute {
                time,
                x,
                y,
                x_extent,
                y_extent,
            } => pending.events.push(PointerEvent::MotionAbsolute {
                time,
                x,
                y,
                x_extent,
                y_extent,
            }),
            Request::Button {
                time,
                button,
                state,
            } => match state {
                WEnum::Value(wl_pointer::ButtonState::Pressed) => {
                    pending.events.push(PointerEvent::Button {
                        time,
                        button,
                        pressed: true,
                    })
                }
                WEnum::Value(wl_pointer::ButtonState::Released) => {
                    pending.events.push(PointerEvent::Button {
                        time,
                        button,
                        pressed: false,
                    })
                }
                WEnum::Unknown(raw) => {
                    tracing::warn!(raw, "ignoring virtual-pointer button with unknown state")
                }
                _ => {}
            },
            Request::Axis { time, axis, value } => {
                pending.axis_time = time;
                match axis {
                    WEnum::Value(wl_pointer::Axis::HorizontalScroll) => {
                        pending.horizontal = Some(value)
                    }
                    WEnum::Value(wl_pointer::Axis::VerticalScroll) => {
                        pending.vertical = Some(value)
                    }
                    WEnum::Unknown(_) => resource.post_error(Error::InvalidAxis, "invalid axis"),
                    _ => {}
                }
            }
            Request::AxisDiscrete {
                time,
                axis,
                value,
                discrete,
            } => {
                pending.axis_time = time;
                match axis {
                    WEnum::Value(wl_pointer::Axis::HorizontalScroll) => {
                        pending.horizontal = Some(value);
                        pending.horizontal_discrete = Some(discrete);
                    }
                    WEnum::Value(wl_pointer::Axis::VerticalScroll) => {
                        pending.vertical = Some(value);
                        pending.vertical_discrete = Some(discrete);
                    }
                    WEnum::Unknown(_) => resource.post_error(Error::InvalidAxis, "invalid axis"),
                    _ => {}
                }
            }
            Request::AxisSource { axis_source } => {
                pending.axis_source = match axis_source {
                    WEnum::Value(wl_pointer::AxisSource::Wheel) => Some(BackendAxisSource::Wheel),
                    WEnum::Value(wl_pointer::AxisSource::Finger) => Some(BackendAxisSource::Finger),
                    WEnum::Value(wl_pointer::AxisSource::Continuous) => {
                        Some(BackendAxisSource::Continuous)
                    }
                    WEnum::Value(wl_pointer::AxisSource::WheelTilt) => {
                        Some(BackendAxisSource::WheelTilt)
                    }
                    WEnum::Unknown(_) => {
                        resource.post_error(Error::InvalidAxisSource, "invalid axis source");
                        None
                    }
                    _ => None,
                };
            }
            Request::AxisStop { time, axis } => {
                pending.axis_time = time;
                match axis {
                    WEnum::Value(wl_pointer::Axis::HorizontalScroll) => {
                        pending.stop_horizontal = true
                    }
                    WEnum::Value(wl_pointer::Axis::VerticalScroll) => pending.stop_vertical = true,
                    WEnum::Unknown(_) => resource.post_error(Error::InvalidAxis, "invalid axis"),
                    _ => {}
                }
            }
            Request::Frame => {
                drop(pending);
                flush(state, data);
            }
            Request::Destroy => {}
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::reexports::wayland_server::Display;

    #[test]
    fn absolute_coordinates_cover_the_target_and_clamp_untrusted_values() {
        assert_eq!(map_absolute_axis(0, 1000, 1920), 0.0);
        assert_eq!(map_absolute_axis(1000, 1000, 1920), 1919.0);
        assert_eq!(map_absolute_axis(5000, 1000, 1920), 1919.0);
        assert_eq!(map_absolute_axis(500, 1000, 1), 0.0);
        assert_eq!(map_absolute_axis(500, 0, 1920), 0.0);
    }

    #[test]
    fn every_session_peer_may_create_a_virtual_pointer() {
        let display = Display::<Compositor>::new().expect("wayland display");
        let mut display_handle = display.handle();
        let (compositor_end, _client_end) =
            std::os::unix::net::UnixStream::pair().expect("socketpair");
        let client = display_handle
            .insert_client(
                compositor_end,
                std::sync::Arc::new(crate::state::ClientState::default()),
            )
            .expect("admit a client");

        assert!(may_create_virtual_pointer(&client));
    }
}
