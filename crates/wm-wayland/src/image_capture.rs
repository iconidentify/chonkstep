//! ext-image-copy-capture-v1 and its output/toplevel source factories.
//!
//! The protocol is deliberately shm-first: the four 32-bit formats the
//! existing screencopy writer already understands are advertised, while
//! dmabuf is omitted until the compositor can prove a zero-copy path. Both
//! outputs and toplevels use the same GLES readback and shm conversion as
//! `zwlr_screencopy_v1`; the long-lived session adds constraint updates and
//! source lifetime tracking around that pixel path.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use smithay::output::Output;
use smithay::reexports::wayland_protocols::ext::image_capture_source::v1::server::{
    ext_foreign_toplevel_image_capture_source_manager_v1::{
        self, ExtForeignToplevelImageCaptureSourceManagerV1,
    },
    ext_image_capture_source_v1::{self, ExtImageCaptureSourceV1},
    ext_output_image_capture_source_manager_v1::{self, ExtOutputImageCaptureSourceManagerV1},
};
use smithay::reexports::wayland_protocols::ext::image_copy_capture::v1::server::{
    ext_image_copy_capture_cursor_session_v1::{self, ExtImageCopyCaptureCursorSessionV1},
    ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1, FailureReason},
    ext_image_copy_capture_manager_v1::{self, ExtImageCopyCaptureManagerV1},
    ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};
use smithay::reexports::wayland_server::protocol::{wl_buffer::WlBuffer, wl_output, wl_shm};
use smithay::reexports::wayland_server::{
    backend::ClientId, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
    WEnum,
};
use smithay::utils::{Clock, Monotonic, Transform};
use smithay::wayland::foreign_toplevel_list::ForeignToplevelHandle;

use wm_theme_api::{Rect, Size};

use crate::state::{Compositor, WlWindowId};

const VERSION: u32 = 1;

#[derive(Clone, Debug)]
enum CaptureTarget {
    Output(String),
    Toplevel(WlWindowId),
}

#[derive(Clone)]
struct CaptureSourceData(Option<CaptureTarget>);

struct SessionShared {
    target: Option<CaptureTarget>,
    paint_cursors: bool,
    latest_constraints: Mutex<Option<Constraints>>,
    frame_live: AtomicBool,
    stopped: AtomicBool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Constraints {
    size: Size,
    /// Re-advertise even where the pixel dimensions happen to stay the
    /// same: scale changes alter the source-to-buffer relationship.
    scale_bits: u64,
}

struct SessionInstance {
    resource: ExtImageCopyCaptureSessionV1,
    shared: Arc<SessionShared>,
}

struct FrameState {
    buffer: Option<WlBuffer>,
    captured: bool,
}

struct FrameData {
    shared: Arc<SessionShared>,
    state: Mutex<FrameState>,
}

struct PendingCapture {
    frame: ExtImageCopyCaptureFrameV1,
    buffer: WlBuffer,
    shared: Arc<SessionShared>,
}

struct CursorSessionData {
    capture_session_created: AtomicBool,
}

pub(crate) struct ImageCapture {
    sessions: Vec<SessionInstance>,
    captures: Vec<PendingCapture>,
}

pub(crate) fn init(display: &DisplayHandle) -> ImageCapture {
    let _copy = display.create_global::<Compositor, ExtImageCopyCaptureManagerV1, ()>(VERSION, ());
    let _output =
        display.create_global::<Compositor, ExtOutputImageCaptureSourceManagerV1, ()>(VERSION, ());
    let _toplevel = display
        .create_global::<Compositor, ExtForeignToplevelImageCaptureSourceManagerV1, ()>(
            VERSION,
            (),
        );
    tracing::info!(
        version = VERSION,
        "ext image-copy-capture and source managers advertised"
    );
    ImageCapture {
        sessions: Vec::new(),
        captures: Vec::new(),
    }
}

/// Re-advertises constraints after resize/modeset, stops dead targets,
/// then answers capture requests parked during protocol dispatch.
pub(crate) fn refresh(comp: &mut Compositor) {
    comp.image_capture
        .sessions
        .retain(|session| session.resource.is_alive());
    let sessions: Vec<_> = comp
        .image_capture
        .sessions
        .iter()
        .map(|session| (session.resource.clone(), Arc::clone(&session.shared)))
        .collect();
    for (resource, shared) in sessions {
        update_constraints(comp, &resource, &shared);
    }

    let pending = std::mem::take(&mut comp.image_capture.captures);
    for capture in pending {
        if !capture.frame.is_alive() {
            continue;
        }
        if capture.shared.stopped.load(Ordering::Relaxed) {
            capture.frame.failed(FailureReason::Stopped);
            continue;
        }
        let Some(constraints) = target_constraints(comp, capture.shared.target.as_ref()) else {
            stop_session(&capture.shared, None);
            capture.frame.failed(FailureReason::Stopped);
            continue;
        };
        if capture.shared.latest_constraints.lock().unwrap().as_ref() != Some(&constraints) {
            capture.frame.failed(FailureReason::BufferConstraints);
            continue;
        }

        // Never bypass the lock screen to read a toplevel's private surface
        // tree. Output capture remains allowed and sees the ordinary locked
        // scene through build_scene.
        if matches!(capture.shared.target, Some(CaptureTarget::Toplevel(_)))
            && comp.wm.backend().locked
        {
            capture.frame.failed(FailureReason::Unknown);
            continue;
        }
        let size = match capture.shared.target.as_ref() {
            Some(CaptureTarget::Output(name)) => {
                let region = comp
                    .outputs
                    .iter()
                    .find(|entry| entry.output.name() == *name)
                    .map(|entry| Rect::new(entry.position, entry.size));
                region.and_then(|region| {
                    match crate::protocols::capture_region_into(
                        comp,
                        region,
                        Transform::Normal,
                        capture.shared.paint_cursors,
                        &capture.buffer,
                    ) {
                        Ok(size) => Some(size),
                        Err(error) => {
                            tracing::warn!(%error, "ext image-copy-capture could not capture the output");
                            None
                        }
                    }
                })
            }
            Some(CaptureTarget::Toplevel(window)) => crate::capture::capture_window_full(
                    comp,
                    *window,
                    capture.shared.paint_cursors,
                )
                .and_then(|pixels| match crate::protocols::write_capture(&capture.buffer, &pixels) {
                    Ok(()) => Some(Size::new(pixels.width, pixels.height)),
                    Err(error) => {
                        tracing::warn!(%error, "ext image-copy-capture could not write the client buffer");
                        None
                    }
                }),
            None => None,
        };
        let Some(size) = size else {
            capture.frame.failed(FailureReason::Unknown);
            continue;
        };

        capture.frame.transform(wl_output::Transform::Normal);
        capture.frame.damage(0, 0, size.w as i32, size.h as i32);
        let stamp = Duration::from(Clock::<Monotonic>::new().now());
        capture.frame.presentation_time(
            (stamp.as_secs() >> 32) as u32,
            stamp.as_secs() as u32,
            stamp.subsec_nanos(),
        );
        capture.frame.ready();
    }
}

fn target_constraints(comp: &Compositor, target: Option<&CaptureTarget>) -> Option<Constraints> {
    match target? {
        CaptureTarget::Output(name) => comp
            .outputs
            .iter()
            .find(|entry| entry.output.name() == *name)
            .filter(|entry| entry.size.w > 0 && entry.size.h > 0)
            .map(|entry| Constraints {
                size: entry.size,
                scale_bits: entry.scale.to_bits(),
            }),
        CaptureTarget::Toplevel(window) => {
            let backend = comp.wm.backend();
            let record = backend
                .windows
                .get(window)
                .filter(|record| record.mapped && record.surface.alive())?;
            (record.content.size.w > 0 && record.content.size.h > 0).then(|| Constraints {
                size: record.content.size,
                scale_bits: backend.window_surface_scale(record).to_bits(),
            })
        }
    }
}

fn update_constraints(
    comp: &Compositor,
    resource: &ExtImageCopyCaptureSessionV1,
    shared: &Arc<SessionShared>,
) {
    if shared.stopped.load(Ordering::Relaxed) {
        return;
    }
    let Some(constraints) = target_constraints(comp, shared.target.as_ref()) else {
        stop_session(shared, Some(resource));
        return;
    };
    let mut latest = shared.latest_constraints.lock().unwrap();
    if latest.as_ref() == Some(&constraints) {
        return;
    }
    *latest = Some(constraints);
    drop(latest);
    for format in [
        wl_shm::Format::Argb8888,
        wl_shm::Format::Xrgb8888,
        wl_shm::Format::Abgr8888,
        wl_shm::Format::Xbgr8888,
    ] {
        resource.shm_format(format);
    }
    resource.buffer_size(constraints.size.w, constraints.size.h);
    resource.done();
}

fn stop_session(shared: &Arc<SessionShared>, resource: Option<&ExtImageCopyCaptureSessionV1>) {
    if !shared.stopped.swap(true, Ordering::Relaxed) {
        if let Some(resource) = resource {
            resource.stopped();
        }
    }
}

fn create_session(
    state: &mut Compositor,
    data_init: &mut DataInit<'_, Compositor>,
    id: New<ExtImageCopyCaptureSessionV1>,
    target: Option<CaptureTarget>,
    paint_cursors: bool,
) {
    let shared = Arc::new(SessionShared {
        target,
        paint_cursors,
        latest_constraints: Mutex::new(None),
        frame_live: AtomicBool::new(false),
        stopped: AtomicBool::new(false),
    });
    let resource = data_init.init(id, Arc::clone(&shared));
    state
        .image_capture
        .sessions
        .push(SessionInstance { resource, shared });
}

impl GlobalDispatch<ExtOutputImageCaptureSourceManagerV1, ()> for Compositor {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ExtOutputImageCaptureSourceManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, _global_data: &()) -> bool {
        crate::state::privileged_global_visible(&client)
    }
}

impl Dispatch<ExtOutputImageCaptureSourceManagerV1, ()> for Compositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ExtOutputImageCaptureSourceManagerV1,
        request: ext_output_image_capture_source_manager_v1::Request,
        _data: &(),
        _handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ext_output_image_capture_source_manager_v1::Request::CreateSource {
                source,
                output,
            } => {
                let target = Output::from_resource(&output)
                    .map(|output| CaptureTarget::Output(output.name()));
                data_init.init(source, CaptureSourceData(target));
            }
            ext_output_image_capture_source_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl GlobalDispatch<ExtForeignToplevelImageCaptureSourceManagerV1, ()> for Compositor {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ExtForeignToplevelImageCaptureSourceManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, _global_data: &()) -> bool {
        crate::state::privileged_global_visible(&client)
    }
}

impl Dispatch<ExtForeignToplevelImageCaptureSourceManagerV1, ()> for Compositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ExtForeignToplevelImageCaptureSourceManagerV1,
        request: ext_foreign_toplevel_image_capture_source_manager_v1::Request,
        _data: &(),
        _handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ext_foreign_toplevel_image_capture_source_manager_v1::Request::CreateSource {
                source,
                toplevel_handle,
            } => {
                let target = ForeignToplevelHandle::from_resource(&toplevel_handle)
                    .and_then(|handle| handle.user_data().get::<WlWindowId>().copied())
                    .map(CaptureTarget::Toplevel);
                data_init.init(source, CaptureSourceData(target));
            }
            ext_foreign_toplevel_image_capture_source_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCaptureSourceV1, CaptureSourceData> for Compositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ExtImageCaptureSourceV1,
        request: ext_image_capture_source_v1::Request,
        _data: &CaptureSourceData,
        _handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let _ = request;
    }
}

impl GlobalDispatch<ExtImageCopyCaptureManagerV1, ()> for Compositor {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ExtImageCopyCaptureManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, _global_data: &()) -> bool {
        crate::state::privileged_global_visible(&client)
    }
}

impl Dispatch<ExtImageCopyCaptureManagerV1, ()> for Compositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ExtImageCopyCaptureManagerV1,
        request: ext_image_copy_capture_manager_v1::Request,
        _data: &(),
        _handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ext_image_copy_capture_manager_v1::Request::CreateSession {
                session,
                source,
                options,
            } => {
                let (paint_cursors, valid) = match options {
                    WEnum::Value(options) => (
                        options.contains(ext_image_copy_capture_manager_v1::Options::PaintCursors),
                        true,
                    ),
                    WEnum::Unknown(_) => (false, false),
                };
                let target = source
                    .data::<CaptureSourceData>()
                    .and_then(|data| data.0.clone());
                create_session(state, data_init, session, target, paint_cursors);
                if !valid {
                    resource.post_error(
                        ext_image_copy_capture_manager_v1::Error::InvalidOption,
                        "unknown image-copy-capture option",
                    );
                }
            }
            ext_image_copy_capture_manager_v1::Request::CreatePointerCursorSession {
                session,
                ..
            } => {
                data_init.init(
                    session,
                    CursorSessionData {
                        capture_session_created: AtomicBool::new(false),
                    },
                );
            }
            ext_image_copy_capture_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, Arc<SessionShared>> for Compositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ExtImageCopyCaptureSessionV1,
        request: ext_image_copy_capture_session_v1::Request,
        data: &Arc<SessionShared>,
        _handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ext_image_copy_capture_session_v1::Request::CreateFrame { frame } => {
                if data.frame_live.swap(true, Ordering::Relaxed) {
                    resource.post_error(
                        ext_image_copy_capture_session_v1::Error::DuplicateFrame,
                        "the previous capture frame is still alive",
                    );
                    return;
                }
                data_init.init(
                    frame,
                    FrameData {
                        shared: Arc::clone(data),
                        state: Mutex::new(FrameState {
                            buffer: None,
                            captured: false,
                        }),
                    },
                );
            }
            ext_image_copy_capture_session_v1::Request::Destroy => {
                state
                    .image_capture
                    .sessions
                    .retain(|session| &session.resource != resource);
            }
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        resource: &ExtImageCopyCaptureSessionV1,
        _data: &Arc<SessionShared>,
    ) {
        state
            .image_capture
            .sessions
            .retain(|session| &session.resource != resource);
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, FrameData> for Compositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ExtImageCopyCaptureFrameV1,
        request: ext_image_copy_capture_frame_v1::Request,
        data: &FrameData,
        _handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let mut frame = data.state.lock().unwrap();
        match request {
            ext_image_copy_capture_frame_v1::Request::Destroy => {
                state
                    .image_capture
                    .captures
                    .retain(|capture| &capture.frame != resource);
                data.shared.frame_live.store(false, Ordering::Relaxed);
            }
            ext_image_copy_capture_frame_v1::Request::AttachBuffer { buffer } => {
                if frame.captured {
                    resource.post_error(
                        ext_image_copy_capture_frame_v1::Error::AlreadyCaptured,
                        "capture has already been requested for this frame",
                    );
                } else {
                    frame.buffer = Some(buffer);
                }
            }
            ext_image_copy_capture_frame_v1::Request::DamageBuffer {
                x,
                y,
                width,
                height,
            } => {
                if frame.captured {
                    resource.post_error(
                        ext_image_copy_capture_frame_v1::Error::AlreadyCaptured,
                        "capture has already been requested for this frame",
                    );
                } else if x < 0 || y < 0 || width <= 0 || height <= 0 {
                    resource.post_error(
                        ext_image_copy_capture_frame_v1::Error::InvalidBufferDamage,
                        "buffer damage must have non-negative origin and positive size",
                    );
                }
                // Full-frame readback is intentional, so valid client damage
                // is accepted but needs no retained region.
            }
            ext_image_copy_capture_frame_v1::Request::Capture => {
                if frame.captured {
                    resource.post_error(
                        ext_image_copy_capture_frame_v1::Error::AlreadyCaptured,
                        "capture has already been requested for this frame",
                    );
                    return;
                }
                let Some(buffer) = frame.buffer.clone() else {
                    resource.post_error(
                        ext_image_copy_capture_frame_v1::Error::NoBuffer,
                        "capture requires an attached buffer",
                    );
                    return;
                };
                frame.captured = true;
                state.image_capture.captures.push(PendingCapture {
                    frame: resource.clone(),
                    buffer,
                    shared: Arc::clone(&data.shared),
                });
                state.wm.backend_mut().mark_damaged();
            }
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        resource: &ExtImageCopyCaptureFrameV1,
        data: &FrameData,
    ) {
        state
            .image_capture
            .captures
            .retain(|capture| &capture.frame != resource);
        data.shared.frame_live.store(false, Ordering::Relaxed);
    }
}

impl Dispatch<ExtImageCopyCaptureCursorSessionV1, CursorSessionData> for Compositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ExtImageCopyCaptureCursorSessionV1,
        request: ext_image_copy_capture_cursor_session_v1::Request,
        data: &CursorSessionData,
        _handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ext_image_copy_capture_cursor_session_v1::Request::GetCaptureSession { session } => {
                if data.capture_session_created.swap(true, Ordering::Relaxed) {
                    resource.post_error(
                        ext_image_copy_capture_cursor_session_v1::Error::DuplicateSession,
                        "cursor capture session already created",
                    );
                    return;
                }
                // Separate cursor-image streams are optional. Create an
                // immediately stopped base session so clients get a complete
                // lifecycle instead of an uninitialised object.
                create_session(state, data_init, session, None, false);
            }
            ext_image_copy_capture_cursor_session_v1::Request::Destroy => {}
            _ => {}
        }
    }
}
