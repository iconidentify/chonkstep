//! Adversarial xdg-activation client for the abandoned-token bound E2E.
//!
//! A normal launcher asks for one token and passes it to the program it
//! starts. This probe asks for many and activates nothing, the exact client
//! behavior that used to grow the compositor's token map for the life of
//! the session.

use std::io::Write;

use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::xdg::activation::v1::client::{
    xdg_activation_token_v1::{self, XdgActivationTokenV1},
    xdg_activation_v1::XdgActivationV1,
};

const REQUESTS: usize = 512;

#[derive(Default)]
struct Probe {
    activation: Option<XdgActivationV1>,
    completed: usize,
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
        let wl_registry::Event::Global { name, interface, version } = event else {
            return;
        };
        if interface == "xdg_activation_v1" {
            probe.activation = Some(registry.bind(name, version.min(1), qh, ()));
        }
    }
}

impl Dispatch<XdgActivationV1, ()> for Probe {
    fn event(
        _: &mut Self,
        _: &XdgActivationV1,
        _: <XdgActivationV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<XdgActivationTokenV1, ()> for Probe {
    fn event(
        probe: &mut Self,
        _: &XdgActivationTokenV1,
        event: xdg_activation_token_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, xdg_activation_token_v1::Event::Done { .. }) {
            probe.completed += 1;
        }
    }
}

fn main() {
    let connection = Connection::connect_to_env().expect("connect to compositor");
    let display = connection.display();
    let mut queue = connection.new_event_queue::<Probe>();
    let qh = queue.handle();
    display.get_registry(&qh, ());
    let mut probe = Probe::default();
    queue.roundtrip(&mut probe).expect("registry roundtrip");

    let activation = probe.activation.clone().expect("xdg-activation global");
    let mut tokens = Vec::with_capacity(REQUESTS);
    for _ in 0..REQUESTS {
        let token = activation.get_activation_token(&qh, ());
        token.commit();
        tokens.push(token);
    }
    queue.roundtrip(&mut probe).expect("token roundtrip");

    println!("**requested {REQUESTS}; completed {}**", probe.completed);
    let _ = std::io::stdout().flush();
    for token in tokens {
        token.destroy();
    }
}
