//! A focused native selection owner/receiver, not a privileged data-control
//! client. F5/F6 copy clipboard/primary, F7/F8 paste, F9/F10 clear ownership.
//! Bytes are private fixture files; transfers report size, never their content.

use std::fs::File;
#[cfg(test)]
use std::io::Read;
use std::io::Write;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_data_device, wl_data_device_manager, wl_data_offer,
    wl_data_source, wl_keyboard, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::wp::primary_selection::zv1::client::{
    zwp_primary_selection_device_manager_v1 as primary_manager,
    zwp_primary_selection_device_v1 as primary_device,
    zwp_primary_selection_offer_v1 as primary_offer,
    zwp_primary_selection_source_v1 as primary_source,
};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

const LIMIT: usize = 16 * 1024 * 1024;

#[path = "chonk-selection-probe/receive.rs"]
mod receive;

fn send_bytes(fd: OwnedFd, mut payload: &[u8]) -> std::io::Result<()> {
    use rustix::event::{poll, PollFd, PollFlags, Timespec};
    use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};
    let mut file = File::from(fd);
    fcntl_setfl(&file, fcntl_getfl(&file)? | OFlags::NONBLOCK)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !payload.is_empty() {
        match file.write(payload) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(count) => payload = &payload[count..],
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .ok_or(std::io::ErrorKind::TimedOut)?;
                let timeout = Timespec::try_from(remaining).unwrap();
                match poll(&mut [PollFd::new(&file, PollFlags::OUT)], Some(&timeout)) {
                    Ok(0) => return Err(std::io::ErrorKind::TimedOut.into()),
                    Ok(_) | Err(rustix::io::Errno::INTR) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn say(message: impl std::fmt::Display) {
    println!("{message}");
    std::io::stdout().flush().expect("flush selection event");
}

#[derive(Default)]
struct Probe {
    closed: bool,
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    shell: Option<xdg_wm_base::XdgWmBase>,
    seat: Option<wl_seat::WlSeat>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    data_manager: Option<wl_data_device_manager::WlDataDeviceManager>,
    data_device: Option<wl_data_device::WlDataDevice>,
    data_offer: Option<wl_data_offer::WlDataOffer>,
    data_source: Option<wl_data_source::WlDataSource>,
    primary_manager: Option<primary_manager::ZwpPrimarySelectionDeviceManagerV1>,
    primary_device: Option<primary_device::ZwpPrimarySelectionDeviceV1>,
    primary_offer: Option<primary_offer::ZwpPrimarySelectionOfferV1>,
    primary_source: Option<primary_source::ZwpPrimarySelectionSourceV1>,
    payload: Arc<[u8]>,
    mime: String,
    output: PathBuf,
    received: usize,
    receive_mode: receive::Mode,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl Probe {
    fn worker(&mut self, action: impl FnOnce() + Send + 'static) {
        // A blocked consumer must not prevent this *client* from dispatching
        // selection/focus events. Bound outstanding work even in abuse cases.
        let mut index = 0;
        while index < self.workers.len() {
            if self.workers[index].is_finished() {
                self.workers
                    .swap_remove(index)
                    .join()
                    .expect("transfer worker");
            } else {
                index += 1;
            }
        }
        assert!(
            self.workers.len() < 16,
            "bounded outstanding test transfers"
        );
        self.workers.push(std::thread::spawn(action));
    }

    fn send(&mut self, fd: OwnedFd, mime: &str) {
        assert_eq!(mime, self.mime, "request an advertised MIME type");
        let payload = self.payload.clone();
        self.worker(move || {
            let result = send_bytes(fd, &payload);
            say(format!("send {} {result:?}", payload.len()));
        });
    }

    fn receive(&mut self, primary: bool) {
        let (reader, writer) = UnixStream::pair().expect("private transfer socket");
        if matches!(self.receive_mode, receive::Mode::Slow | receive::Mode::Gated) {
            // A real receiver controls its destination FD. Keep this below an
            // INCR chunk so even a lightly loaded host exercises partial writes
            // instead of absorbing the whole chunk in a large socket buffer.
            rustix::net::sockopt::set_socket_send_buffer_size(&writer, 4096)
                .expect("bound the private consumer socket buffer");
        }
        reader
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        if primary {
            let Some(offer) = self.primary_offer.as_ref() else {
                say("primary absent");
                return;
            };
            offer.receive(self.mime.clone(), writer.as_fd());
        } else {
            let Some(offer) = self.data_offer.as_ref() else {
                say("clipboard absent");
                return;
            };
            offer.receive(self.mime.clone(), writer.as_fd());
        }
        drop(writer);
        self.received += 1;
        let kind = if primary { "primary" } else { "clipboard" };
        let path = self.output.join(format!("{kind}-{}.bin", self.received));
        let directory = self.output.clone();
        let mode = self.receive_mode;
        let sequence = self.received;
        self.worker(
            move || match receive::bytes(reader, mode, &directory, sequence, kind) {
                Ok(Some(bytes)) => {
                    std::fs::write(&path, &bytes).expect("record exact received bytes");
                    say(format!("received {kind} {}", bytes.len()));
                }
                Ok(None) => {}
                Err(error) => say(format!("receive-error {kind} {error}")),
            },
        );
    }

    fn key(&mut self, key: u32, serial: u32, qh: &QueueHandle<Self>) {
        match key {
            63 => {
                let source = self
                    .data_manager
                    .as_ref()
                    .unwrap()
                    .create_data_source(qh, ());
                source.offer(self.mime.clone());
                self.data_device
                    .as_ref()
                    .unwrap()
                    .set_selection(Some(&source), serial);
                self.data_source = Some(source);
                say("clipboard published");
            }
            64 => {
                let source = self.primary_manager.as_ref().unwrap().create_source(qh, ());
                source.offer(self.mime.clone());
                self.primary_device
                    .as_ref()
                    .unwrap()
                    .set_selection(Some(&source), serial);
                self.primary_source = Some(source);
                say("primary published");
            }
            65 => self.receive(false),
            66 => self.receive(true),
            67 => self
                .data_device
                .as_ref()
                .unwrap()
                .set_selection(None, serial),
            68 => self
                .primary_device
                .as_ref()
                .unwrap()
                .set_selection(None, serial),
            _ => {}
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for Probe {
    fn event(
        probe: &mut Self,
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
                "wl_compositor" => {
                    probe.compositor = Some(registry.bind(name, version.min(4), qh, ()))
                }
                "wl_shm" => probe.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => probe.shell = Some(registry.bind(name, version.min(3), qh, ())),
                "wl_seat" => probe.seat = Some(registry.bind(name, version.min(7), qh, ())),
                "wl_data_device_manager" => {
                    probe.data_manager = Some(registry.bind(name, version.min(3), qh, ()))
                }
                "zwp_primary_selection_device_manager_v1" => {
                    probe.primary_manager = Some(registry.bind(name, 1, qh, ()))
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Probe {
    fn event(
        probe: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities } = event {
            if capabilities
                .into_result()
                .unwrap()
                .contains(wl_seat::Capability::Keyboard)
                && probe.keyboard.is_none()
            {
                probe.keyboard = Some(seat.get_keyboard(qh, ()));
            }
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Probe {
    fn event(
        probe: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { .. } => say("keyboard enter"),
            wl_keyboard::Event::Leave { .. } => say("keyboard leave"),
            wl_keyboard::Event::Key {
                key, state, serial, ..
            } if state.into_result().unwrap() == wl_keyboard::KeyState::Pressed => {
                probe.key(key, serial, qh)
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_data_device::WlDataDevice, ()> for Probe {
    fn event(
        probe: &mut Self,
        _: &wl_data_device::WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_device::Event::Selection { id } = event {
            if let Some(old) = probe.data_offer.take() {
                old.destroy();
            }
            say(format!("clipboard offer {}", id.is_some()));
            probe.data_offer = id;
        }
    }
    wayland_client::event_created_child!(Probe, wl_data_device::WlDataDevice, [0 => (wl_data_offer::WlDataOffer, ())]);
}

impl Dispatch<primary_device::ZwpPrimarySelectionDeviceV1, ()> for Probe {
    fn event(
        probe: &mut Self,
        _: &primary_device::ZwpPrimarySelectionDeviceV1,
        event: primary_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let primary_device::Event::Selection { id } = event {
            if let Some(old) = probe.primary_offer.take() {
                old.destroy();
            }
            say(format!("primary offer {}", id.is_some()));
            probe.primary_offer = id;
        }
    }
    wayland_client::event_created_child!(Probe, primary_device::ZwpPrimarySelectionDeviceV1, [0 => (primary_offer::ZwpPrimarySelectionOfferV1, ())]);
}

impl Dispatch<wl_data_source::WlDataSource, ()> for Probe {
    fn event(
        probe: &mut Self,
        source: &wl_data_source::WlDataSource,
        event: wl_data_source::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_source::Event::Send { mime_type, fd } => probe.send(fd, &mime_type),
            wl_data_source::Event::Cancelled => {
                if probe.data_source.as_ref() == Some(source) {
                    probe.data_source = None;
                }
                source.destroy();
                say("clipboard cancelled");
            }
            _ => {}
        }
    }
}

impl Dispatch<primary_source::ZwpPrimarySelectionSourceV1, ()> for Probe {
    fn event(
        probe: &mut Self,
        source: &primary_source::ZwpPrimarySelectionSourceV1,
        event: primary_source::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            primary_source::Event::Send { mime_type, fd } => probe.send(fd, &mime_type),
            primary_source::Event::Cancelled => {
                if probe.primary_source.as_ref() == Some(source) {
                    probe.primary_source = None;
                }
                source.destroy();
                say("primary cancelled");
            }
            _ => {}
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Probe {
    fn event(
        _: &mut Self,
        shell: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            shell.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for Probe {
    fn event(
        _: &mut Self,
        surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for Probe {
    fn event(state: &mut Self, _: &xdg_toplevel::XdgToplevel, event: xdg_toplevel::Event,
        _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if matches!(event, xdg_toplevel::Event::Close) { state.closed = true; }
    }
}

macro_rules! ignore_events {
    ($($proxy:ty),* $(,)?) => {$(
        wayland_client::delegate_noop!(Probe: ignore $proxy);
    )*};
}

ignore_events!(
    wl_compositor::WlCompositor,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_buffer::WlBuffer,
    wl_surface::WlSurface,
    wl_data_device_manager::WlDataDeviceManager,
    wl_data_offer::WlDataOffer,
    primary_manager::ZwpPrimarySelectionDeviceManagerV1,
    primary_offer::ZwpPrimarySelectionOfferV1
);

fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert!(
        (5..=6).contains(&args.len()),
        "title MIME input-file output-directory [slow|gated]"
    );
    let payload = std::fs::read(&args[3]).expect("private input fixture");
    assert!(payload.len() <= LIMIT, "bounded fixture");
    let mut probe = Probe {
        payload: payload.into(),
        mime: args[2].clone(),
        output: PathBuf::from(&args[4]),
        receive_mode: receive::Mode::parse(args.get(5).map(String::as_str)),
        ..Default::default()
    };
    let connection = Connection::connect_to_env().expect("private Wayland session");
    let mut queue = connection.new_event_queue::<Probe>();
    let qh = queue.handle();
    connection.display().get_registry(&qh, ());
    queue.roundtrip(&mut probe).expect("registry");
    queue.roundtrip(&mut probe).expect("seat capabilities");
    let seat = probe.seat.as_ref().expect("seat");
    probe.data_device = Some(
        probe
            .data_manager
            .as_ref()
            .unwrap()
            .get_data_device(seat, &qh, ()),
    );
    probe.primary_device = Some(
        probe
            .primary_manager
            .as_ref()
            .unwrap()
            .get_device(seat, &qh, ()),
    );
    let surface = probe.compositor.as_ref().unwrap().create_surface(&qh, ());
    let xdg = probe
        .shell
        .as_ref()
        .unwrap()
        .get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title(args[1].clone());
    toplevel.set_app_id(args[1].clone());
    toplevel.set_min_size(320, 200);
    toplevel.set_max_size(320, 200);
    surface.commit();
    queue.roundtrip(&mut probe).expect("initial configure");
    let path = probe.output.join("surface.shm");
    let mut file = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    let pixels: Vec<u8> = std::iter::repeat_n([0x50, 0x70, 0x30, 0xff], 320 * 200)
        .flatten()
        .collect();
    file.write_all(&pixels).unwrap();
    let pool = probe
        .shm
        .as_ref()
        .unwrap()
        .create_pool(file.as_fd(), pixels.len() as i32, &qh, ());
    let buffer = pool.create_buffer(0, 320, 200, 320 * 4, wl_shm::Format::Argb8888, &qh, ());
    surface.attach(Some(&buffer), 0, 0);
    surface.damage_buffer(0, 0, 320, 200);
    surface.commit();
    while !probe.closed {
        queue
            .blocking_dispatch(&mut probe)
            .expect("selection dispatch");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonblocking_sender_preserves_bytes_across_backpressure() {
        let (mut reader, writer) = UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let expected: Vec<_> = (0u8..=255).cycle().take(1024 * 1024 + 17).collect();
        let payload = expected.clone();
        let task = std::thread::spawn(move || send_bytes(writer.into(), &payload));
        let mut received = Vec::new();
        reader.read_to_end(&mut received).unwrap();
        task.join().unwrap().unwrap();
        assert!(received == expected);
    }

    #[test]
    fn sender_reports_a_closed_consumer_without_hanging() {
        let (reader, writer) = UnixStream::pair().unwrap();
        drop(reader);
        assert_eq!(
            send_bytes(writer.into(), b"cancelled").unwrap_err().kind(),
            std::io::ErrorKind::BrokenPipe
        );
    }

    #[test]
    fn an_empty_selection_is_a_successful_empty_transfer() {
        let (mut reader, writer) = UnixStream::pair().unwrap();
        send_bytes(writer.into(), b"").unwrap();
        assert_eq!(reader.read(&mut [0]).unwrap(), 0);
    }
}
