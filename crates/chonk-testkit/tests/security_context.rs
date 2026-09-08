//! Registry and forged-bind checks through an actual security-context listener.
use chonk_testkit::{Session, SessionOptions};
use rustix::event::{poll, PollFd, PollFlags, Timespec};
use std::collections::HashMap;
use std::os::fd::AsFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::{Duration, Instant};
use wayland_client::protocol::{wl_callback, wl_compositor, wl_registry, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::wp::security_context::v1::client::{
    wp_security_context_manager_v1, wp_security_context_v1,
};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1;

#[derive(Default)]
struct Registry {
    globals: HashMap<String, (u32, u32)>,
    done: bool,
}
impl Dispatch<wl_registry::WlRegistry, ()> for Registry {
    fn event(
        state: &mut Self,
        _: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            state.globals.insert(interface, (name, version));
        }
    }
}
impl Dispatch<wl_callback::WlCallback, ()> for Registry {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.done = true;
    }
}
macro_rules! ignore_events {
    ($($proxy:ty),* $(,)?) => {$(
        impl Dispatch<$proxy, ()> for Registry {
            fn event(_: &mut Self, _: &$proxy, _: <$proxy as Proxy>::Event,
                _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore_events!(
    wp_security_context_manager_v1::WpSecurityContextManagerV1,
    wp_security_context_v1::WpSecurityContextV1,
    wl_compositor::WlCompositor,
    wl_surface::WlSurface,
    zwlr_layer_shell_v1::ZwlrLayerShellV1
);

struct Client {
    connection: Connection,
    queue: EventQueue<Registry>,
    state: Registry,
    registry: wl_registry::WlRegistry,
}
impl Client {
    fn connect(stream: UnixStream) -> Self {
        let connection = Connection::from_socket(stream).unwrap();
        let queue = connection.new_event_queue();
        let registry = connection.display().get_registry(&queue.handle(), ());
        let mut client = Self {
            connection,
            queue,
            state: Registry::default(),
            registry,
        };
        client.sync().unwrap();
        client
    }
    fn sync(&mut self) -> Result<(), String> {
        self.state.done = false;
        self.connection.display().sync(&self.queue.handle(), ());
        self.connection.flush().map_err(|e| e.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.state.done {
            self.queue
                .dispatch_pending(&mut self.state)
                .map_err(|e| e.to_string())?;
            if self.state.done {
                break;
            }
            if Instant::now() >= deadline {
                return Err("private display sync timed out".into());
            }
            if let Some(reader) = self.queue.prepare_read() {
                let timeout = Timespec::try_from(Duration::from_millis(10)).unwrap();
                if poll(
                    &mut [PollFd::new(&self.connection, PollFlags::IN)],
                    Some(&timeout),
                )
                .unwrap()
                    != 0
                {
                    reader.read().map_err(|e| e.to_string())?;
                }
            }
        }
        Ok(())
    }
}

#[test]
#[ignore = "needs nested Wayland: scripts/e2e.sh --headless --test security_context"]
fn confined_clients_keep_application_globals_but_cannot_bind_desktop_privileges() {
    let session = Session::boot("security-context-boundary", SessionOptions::default()).unwrap();
    let socket = std::path::PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap())
        .join(&session.wayland_display);
    let mut creator = Client::connect(UnixStream::connect(socket).unwrap());
    let (name, _) = creator.state.globals["wp_security_context_manager_v1"];
    let manager: wp_security_context_manager_v1::WpSecurityContextManagerV1 =
        creator.registry.bind(name, 1, &creator.queue.handle(), ());
    let path = session.dir.join("confined.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let (close_fd, _keep_open) = UnixStream::pair().unwrap();
    let context = manager.create_listener(
        listener.as_fd(),
        close_fd.as_fd(),
        &creator.queue.handle(),
        (),
    );
    context.set_sandbox_engine("audit-test".into());
    context.set_app_id("org.chonkstep.confined-probe".into());
    context.commit();
    creator.sync().unwrap();
    let mut confined = Client::connect(UnixStream::connect(&path).unwrap());
    for interface in [
        "wl_compositor",
        "wl_shm",
        "wl_seat",
        "wl_output",
        "xdg_wm_base",
        "wl_data_device_manager",
    ] {
        assert!(
            confined.state.globals.contains_key(interface),
            "ordinary application capability missing: {interface}"
        );
    }
    for interface in [
        "wp_security_context_manager_v1",
        "zwlr_layer_shell_v1",
        "ext_session_lock_manager_v1",
        "zwlr_screencopy_manager_v1",
        "ext_image_copy_capture_manager_v1",
        "zwlr_data_control_manager_v1",
        "ext_data_control_manager_v1",
        "zwp_virtual_keyboard_manager_v1",
        "zwlr_virtual_pointer_manager_v1",
        "zwlr_output_manager_v1",
        "zwlr_foreign_toplevel_manager_v1",
        "zwp_input_method_manager_v2",
    ] {
        assert!(
            creator.state.globals.contains_key(interface),
            "test must exercise an implemented global: {interface}"
        );
        assert!(
            !confined.state.globals.contains_key(interface),
            "sandbox escaped through {interface}"
        );
    }
    // These are advertised only by the native DRM backend. The shared
    // predicate still covers their bind implementations; a nested run cannot
    // pretend to exercise native-only globals.
    for interface in [
        "zwlr_output_power_manager_v1",
        "zwlr_gamma_control_manager_v1",
    ] {
        if creator.state.globals.contains_key(interface) {
            assert!(!confined.state.globals.contains_key(interface));
        }
    }
    let (name, _) = confined.state.globals["wl_compositor"];
    let compositor: wl_compositor::WlCompositor =
        confined
            .registry
            .bind(name, 4, &confined.queue.handle(), ());
    compositor
        .create_surface(&confined.queue.handle(), ())
        .commit();
    confined.sync().unwrap();
    // Knowing a global's numeric ID from an ordinary connection must not
    // bypass the registry filter when a confined connection forges a bind.
    let (name, _) = creator.state.globals["zwlr_layer_shell_v1"];
    let _: zwlr_layer_shell_v1::ZwlrLayerShellV1 =
        confined
            .registry
            .bind(name, 1, &confined.queue.handle(), ());
    let error = confined
        .sync()
        .expect_err("forged privileged bind must disconnect the confined client");
    assert!(
        !error.contains("timed out"),
        "a timeout is not proof of bind rejection"
    );
    creator.sync().unwrap();
}
