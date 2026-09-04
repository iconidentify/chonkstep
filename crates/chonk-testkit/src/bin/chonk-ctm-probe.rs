//! Scriptable `hyprland-ctm-control-v1` client for end-to-end tests.
//!
//! This deliberately talks the same protocol as `hyprsunset`, while
//! exposing the cases a daemon cannot conveniently be asked to send:
//! a second manager, an off-diagonal matrix, and a client that is
//! killed while its transform is live.

use std::io::Write;

use wayland_client::protocol::{wl_output::WlOutput, wl_registry};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};

mod ctm_bindings {
    // The scanner expands references through `super::wayland_client`;
    // this apparently redundant import is part of its generated API.
    #[allow(clippy::single_component_path_imports)]
    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::backend as wayland_backend;
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!(
            "../wm-wayland/protocols/hyprland-ctm-control-v1.xml"
        );
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("../wm-wayland/protocols/hyprland-ctm-control-v1.xml");
}

use ctm_bindings::hyprland_ctm_control_manager_v1::{self, HyprlandCtmControlManagerV1};

fn fatal(message: &str) -> ! {
    eprintln!("chonk-ctm-probe: {message}");
    std::process::exit(1);
}

#[derive(Default)]
struct Probe {
    manager: Option<HyprlandCtmControlManagerV1>,
    manager_global: Option<(u32, u32)>,
    outputs: Vec<WlOutput>,
    blocked: Vec<u32>,
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
                "wl_output" => probe
                    .outputs
                    .push(registry.bind(name, version.min(4), qh, ())),
                "hyprland_ctm_control_manager_v1" => {
                    let version = version.min(2);
                    probe.manager_global = Some((name, version));
                    probe.manager = Some(registry.bind(name, version, qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<HyprlandCtmControlManagerV1, ()> for Probe {
    fn event(
        probe: &mut Self,
        manager: &HyprlandCtmControlManagerV1,
        event: hyprland_ctm_control_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, hyprland_ctm_control_manager_v1::Event::Blocked) {
            probe.blocked.push(manager.id().protocol_id());
        }
    }
}

impl Dispatch<WlOutput, ()> for Probe {
    fn event(
        _: &mut Self,
        _: &WlOutput,
        _: <WlOutput as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

fn set(manager: &HyprlandCtmControlManagerV1, output: &WlOutput, matrix: [f64; 9]) {
    manager.set_ctm_for_output(
        output, matrix[0], matrix[1], matrix[2], matrix[3], matrix[4], matrix[5], matrix[6],
        matrix[7], matrix[8],
    );
    manager.commit();
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "report".into());
    let connection = Connection::connect_to_env()
        .unwrap_or_else(|error| fatal(&format!("cannot connect: {error}")));
    let display = connection.display();
    let mut queue = connection.new_event_queue::<Probe>();
    let qh = queue.handle();
    let registry = display.get_registry(&qh, ());
    let mut probe = Probe::default();
    queue
        .roundtrip(&mut probe)
        .unwrap_or_else(|error| fatal(&format!("registry roundtrip: {error}")));

    let Some(manager) = probe.manager.clone() else {
        println!("**no CTM global**");
        return;
    };
    println!(
        "**CTM global v{}**, {} output(s)",
        manager.version(),
        probe.outputs.len()
    );
    let output = probe
        .outputs
        .first()
        .cloned()
        .unwrap_or_else(|| fatal("no output"));

    match mode.as_str() {
        "report" => {}
        "set" | "hold" => {
            set(
                &manager,
                &output,
                [1.0, 0.0, 0.0, 0.0, 0.70, 0.0, 0.0, 0.0, 0.40],
            );
            queue
                .roundtrip(&mut probe)
                .unwrap_or_else(|error| fatal(&format!("diagonal CTM refused: {error}")));
            println!("**diagonal CTM accepted**");
            if mode == "hold" {
                println!("**holding**");
                let _ = std::io::stdout().flush();
                loop {
                    if queue.blocking_dispatch(&mut probe).is_err() {
                        return;
                    }
                }
            }
        }
        "non-diagonal" => {
            set(
                &manager,
                &output,
                [1.0, 0.125, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            );
            match queue.roundtrip(&mut probe) {
                Ok(_) => println!("**non-diagonal CTM ACCEPTED**"),
                Err(error) => println!("**non-diagonal CTM refused: {error}**"),
            }
        }
        "exclusive" => {
            let (name, version) = probe
                .manager_global
                .unwrap_or_else(|| fatal("manager global disappeared"));
            let second: HyprlandCtmControlManagerV1 = registry.bind(name, version, &qh, ());
            queue
                .roundtrip(&mut probe)
                .unwrap_or_else(|error| fatal(&format!("second bind roundtrip: {error}")));
            if probe.blocked.contains(&second.id().protocol_id()) {
                println!("**second manager blocked**");
            } else {
                println!("**second manager NOT blocked**");
            }
            second.destroy();
        }
        other => fatal(&format!("unknown mode {other}")),
    }

    let _ = std::io::stdout().flush();
}
