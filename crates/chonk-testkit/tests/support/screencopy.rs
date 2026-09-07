//! A native wlr-screencopy client: region requests reach the compositor rather
//! than being cropped by a screenshot utility after a full-output download.
#![allow(clippy::disallowed_methods)]

use chonk_testkit::Session;
use rustix::event::{poll, PollFd, PollFlags, Timespec};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::os::fd::AsFd;
use std::os::unix::{fs::FileExt, net::UnixStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_output, wl_registry, wl_shm, wl_shm_pool,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1 as frame, zwlr_screencopy_manager_v1 as manager,
};

#[derive(Default)]
pub struct Probe {
    output: Option<wl_output::WlOutput>,
    output_transform: Option<wl_output::Transform>,
    shm: Option<wl_shm::WlShm>,
    manager: Option<manager::ZwlrScreencopyManagerV1>,
    sizes: HashMap<usize, (u32, u32, u32)>,
    pub ready: HashSet<usize>,
    pub failed: HashSet<usize>,
    syncs: HashSet<usize>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Probe {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_output" if state.output.is_none() => {
                    state.output = Some(registry.bind(name, version.min(3), qh, ()))
                }
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                "zwlr_screencopy_manager_v1" => {
                    state.manager = Some(registry.bind(name, version.min(3), qh, ()))
                }
                _ => {}
            }
        }
    }
}
impl Dispatch<wl_callback::WlCallback, usize> for Probe {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        id: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.syncs.insert(*id);
    }
}
impl Dispatch<wl_output::WlOutput, ()> for Probe {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Geometry { transform, .. } = event {
            state.output_transform = Some(match transform {
                wayland_client::WEnum::Value(transform) => transform,
                wayland_client::WEnum::Unknown(value) => panic!("unknown output transform {value}"),
            });
        }
    }
}
impl Dispatch<frame::ZwlrScreencopyFrameV1, usize> for Probe {
    fn event(
        state: &mut Self,
        _: &frame::ZwlrScreencopyFrameV1,
        event: frame::Event,
        id: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            frame::Event::Buffer {
                format,
                width,
                height,
                stride,
            } => {
                assert_eq!(
                    format,
                    wayland_client::WEnum::Value(wl_shm::Format::Xrgb8888)
                );
                state.sizes.insert(*id, (width, height, stride));
            }
            frame::Event::Ready { .. } => {
                state.ready.insert(*id);
            }
            frame::Event::Failed => {
                state.failed.insert(*id);
            }
            _ => {}
        }
    }
}
macro_rules! ignore_events {
    ($($proxy:ty),* $(,)?) => {$(
        impl Dispatch<$proxy, ()> for Probe {
            fn event(_: &mut Self, _: &$proxy, _: <$proxy as Proxy>::Event,
                _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore_events!(
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_buffer::WlBuffer,
    manager::ZwlrScreencopyManagerV1
);

pub struct Client {
    connection: Connection,
    queue: EventQueue<Probe>,
    pub probe: Probe,
    serial: usize,
}
pub struct Frame {
    pub resource: frame::ZwlrScreencopyFrameV1,
    pub id: usize,
    pub size: (u32, u32),
    stride: u32,
    transform: wl_output::Transform,
}
pub struct Storage {
    pub pool: wl_shm_pool::WlShmPool,
    pub file: File,
}
impl Client {
    pub fn connect(session: &Session) -> Self {
        let socket = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap())
            .join(&session.wayland_display);
        let connection = Connection::from_socket(UnixStream::connect(socket).unwrap()).unwrap();
        let queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        let mut client = Self {
            connection,
            queue,
            probe: Probe::default(),
            serial: 0,
        };
        client.sync();
        client.sync();
        client
    }
    pub fn until(&mut self, what: &str, condition: impl Fn(&Probe) -> bool) {
        self.until_for(Duration::from_secs(10), what, condition);
    }
    pub fn until_for(
        &mut self,
        duration: Duration,
        what: &str,
        condition: impl Fn(&Probe) -> bool,
    ) {
        let deadline = Instant::now() + duration;
        while !condition(&self.probe) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            self.queue.dispatch_pending(&mut self.probe).unwrap();
            self.connection.flush().unwrap();
            if let Some(reader) = self.queue.prepare_read() {
                let timeout = Timespec::try_from(Duration::from_millis(10)).unwrap();
                if poll(
                    &mut [PollFd::new(&self.connection, PollFlags::IN)],
                    Some(&timeout),
                )
                .unwrap()
                    > 0
                {
                    match reader.read() {
                        Ok(_) => {}
                        Err(wayland_client::backend::WaylandError::Io(error))
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                            ) => {}
                        Err(error) => panic!("Wayland read failed: {error}"),
                    }
                }
            }
            self.queue.dispatch_pending(&mut self.probe).unwrap();
        }
    }
    pub fn sync(&mut self) {
        self.serial += 1;
        let id = self.serial;
        self.connection.display().sync(&self.queue.handle(), id);
        self.until("sync", |probe| probe.syncs.contains(&id));
    }
    pub fn region(&mut self, x: i32, y: i32, width: i32, height: i32) -> Frame {
        self.serial += 1;
        let id = self.serial;
        let resource = self.probe.manager.as_ref().unwrap().capture_output_region(
            0,
            self.probe.output.as_ref().unwrap(),
            x,
            y,
            width,
            height,
            &self.queue.handle(),
            id,
        );
        self.until("capture constraints", |probe| {
            probe.sizes.contains_key(&id) || probe.failed.contains(&id)
        });
        let &(w, h, stride) = self
            .probe
            .sizes
            .get(&id)
            .expect("valid region must advertise pixels");
        Frame {
            resource,
            id,
            size: (w, h),
            stride,
            transform: self
                .probe
                .output_transform
                .expect("output geometry precedes capture"),
        }
    }
    pub fn storage(&self, frame: &Frame) -> Storage {
        let file = tempfile::tempfile().unwrap();
        let length = frame.stride * frame.size.1;
        file.set_len(u64::from(length)).unwrap();
        let pool = self.probe.shm.as_ref().unwrap().create_pool(
            file.as_fd(),
            length as i32,
            &self.queue.handle(),
            (),
        );
        Storage { pool, file }
    }
    pub fn buffer(&self, storage: &Storage, frame: &Frame) -> wl_buffer::WlBuffer {
        storage.pool.create_buffer(
            0,
            frame.size.0 as i32,
            frame.size.1 as i32,
            frame.stride as i32,
            wl_shm::Format::Xrgb8888,
            &self.queue.handle(),
            (),
        )
    }
}
impl Frame {
    /// Return upright output pixels, matching grim's PNG orientation. Buffer
    /// allocation still uses the advertised transformed size and stride above.
    pub fn pixels(&self, storage: &Storage) -> Vec<[u8; 4]> {
        let mut bytes = vec![0; (self.stride * self.size.1) as usize];
        storage.file.read_exact_at(&mut bytes, 0).unwrap();
        upright_pixels(&bytes, self.size, self.stride, self.transform)
    }
}

fn upright_pixels(
    bytes: &[u8],
    (width, height): (u32, u32),
    stride: u32,
    transform: wl_output::Transform,
) -> Vec<[u8; 4]> {
    use wl_output::Transform;
    let (image_width, image_height) = match transform {
        Transform::_90 | Transform::_270 | Transform::Flipped90 | Transform::Flipped270 => {
            (height, width)
        }
        _ => (width, height),
    };
    let mut pixels = Vec::with_capacity(width as usize * height as usize);
    for y in 0..image_height {
        for x in 0..image_width {
            // Map the upright pixel to its transformed buffer location. The
            // flip is relative to this crop, not to the full output height.
            // In particular winit's Flipped180 stores the last crop row first.
            let (bx, by) = match transform {
                Transform::Normal => (x, y),
                Transform::_90 => (width - 1 - y, x),
                Transform::_180 => (width - 1 - x, height - 1 - y),
                Transform::_270 => (y, height - 1 - x),
                Transform::Flipped => (width - 1 - x, y),
                Transform::Flipped90 => (width - 1 - y, height - 1 - x),
                Transform::Flipped180 => (x, height - 1 - y),
                Transform::Flipped270 => (y, x),
                _ => panic!("unsupported output transform {transform:?}"),
            };
            let offset = by as usize * stride as usize + bx as usize * 4;
            let pixel = bytes[offset..offset + 4].try_into().unwrap();
            let [blue, green, red, _] = u32::from_ne_bytes(pixel).to_le_bytes();
            pixels.push([red, green, blue, 255]);
        }
    }
    pixels
}

#[test]
fn output_transforms_normalize_asymmetric_padded_buffer_pixels() {
    use wl_output::Transform;
    // Raw buffer rows ABC / DEF, with padding which must never become pixels.
    let mut bytes = Vec::new();
    for row in [[1u32, 2, 3], [4, 5, 6]] {
        for red in row {
            bytes.extend_from_slice(&(red << 16).to_ne_bytes());
        }
        bytes.extend_from_slice(&[0xFE; 4]);
    }
    for (transform, expected) in [
        (Transform::Normal, [1, 2, 3, 4, 5, 6]),
        (Transform::_90, [3, 6, 2, 5, 1, 4]),
        (Transform::_180, [6, 5, 4, 3, 2, 1]),
        (Transform::_270, [4, 1, 5, 2, 6, 3]),
        (Transform::Flipped, [3, 2, 1, 6, 5, 4]),
        (Transform::Flipped90, [6, 3, 5, 2, 4, 1]),
        (Transform::Flipped180, [4, 5, 6, 1, 2, 3]),
        (Transform::Flipped270, [1, 4, 2, 5, 3, 6]),
    ] {
        let actual = upright_pixels(&bytes, (3, 2), 16, transform);
        assert_eq!(
            actual,
            expected.map(|red| [red, 0, 0, 255]),
            "{transform:?}"
        );
    }
}
