//! Creates real selection devices, then releases them or exits without protocol
//! destructors. File gates let the parent observe both live and dead ownership.
//! No selection sources, offers with payloads, windows or user clipboard access.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use chonk_testkit::poll_until;
use wayland_client::protocol::{
    wl_data_device::WlDataDevice, wl_data_device_manager::WlDataDeviceManager,
    wl_data_offer::WlDataOffer, wl_registry, wl_seat::WlSeat,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1::ExtDataControlDeviceV1,
    ext_data_control_manager_v1::ExtDataControlManagerV1,
    ext_data_control_offer_v1::ExtDataControlOfferV1,
};
use wayland_protocols::wp::primary_selection::zv1::client::{
    zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1,
    zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1,
    zwp_primary_selection_offer_v1::ZwpPrimarySelectionOfferV1,
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::ZwlrDataControlDeviceV1,
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
    zwlr_data_control_offer_v1::ZwlrDataControlOfferV1,
};

#[derive(Default)]
struct Probe {
    legacy: bool,
    seat: Option<WlSeat>,
    core: Option<WlDataDeviceManager>,
    primary: Option<ZwpPrimarySelectionDeviceManagerV1>,
    wlr: Option<ZwlrDataControlManagerV1>,
    ext: Option<ExtDataControlManagerV1>,
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
                "wl_seat" => probe.seat = Some(registry.bind(name, version.min(7), qh, ())),
                "wl_data_device_manager" => {
                    let requested = if probe.legacy { 1 } else { 3 };
                    probe.core = Some(registry.bind(name, version.min(requested), qh, ()));
                }
                "zwp_primary_selection_device_manager_v1" => {
                    probe.primary = Some(registry.bind(name, version.min(1), qh, ()));
                }
                "zwlr_data_control_manager_v1" => {
                    probe.wlr = Some(registry.bind(name, version.min(2), qh, ()));
                }
                "ext_data_control_manager_v1" => {
                    probe.ext = Some(registry.bind(name, version.min(1), qh, ()));
                }
                _ => {}
            }
        }
    }
}

macro_rules! device {
    ($device:ty, $offer:ty) => {
        impl Dispatch<$device, ()> for Probe {
            fn event(
                _: &mut Self, _: &$device, _: <$device as Proxy>::Event,
                _: &(), _: &Connection, _: &QueueHandle<Self>,
            ) {}
            wayland_client::event_created_child!(Probe, $device, [0 => ($offer, ())]);
        }
    };
}
device!(WlDataDevice, WlDataOffer);
device!(ZwpPrimarySelectionDeviceV1, ZwpPrimarySelectionOfferV1);
device!(ZwlrDataControlDeviceV1, ZwlrDataControlOfferV1);
device!(ExtDataControlDeviceV1, ExtDataControlOfferV1);

macro_rules! ignore {
    ($($proxy:ty),+ $(,)?) => {$(wayland_client::delegate_noop!(Probe: ignore $proxy);)+};
}
ignore!(
    WlSeat,
    WlDataDeviceManager,
    WlDataOffer,
    ZwpPrimarySelectionDeviceManagerV1,
    ZwpPrimarySelectionOfferV1,
    ZwlrDataControlManagerV1,
    ZwlrDataControlOfferV1,
    ExtDataControlManagerV1,
    ExtDataControlOfferV1,
);

fn gate(directory: &Path, name: &str) {
    poll_until(Duration::from_secs(30), name, || {
        directory.join(name).exists().then_some(())
    })
    .expect("parent releases the bounded fixture gate");
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert_eq!(args.len(), 4, "mode count private-gate-directory");
    let mode = args[1].as_str();
    assert!(matches!(
        mode,
        "disconnect" | "killed" | "release" | "seat-first" | "legacy"
    ));
    let count: usize = args[2].parse().unwrap();
    assert!((1..=512).contains(&count), "bounded test device count");
    let directory = Path::new(&args[3]);
    let connection = Connection::connect_to_env().expect("private Wayland connection");
    let mut queue = connection.new_event_queue::<Probe>();
    let qh = queue.handle();
    connection.display().get_registry(&qh, ());
    let mut probe = Probe {
        legacy: mode == "legacy",
        ..Default::default()
    };
    queue.roundtrip(&mut probe).unwrap();
    let seat = probe.seat.as_ref().expect("seat").clone();
    let mut devices = Vec::with_capacity(count);
    for _ in 0..count {
        devices.push((
            probe
                .core
                .as_ref()
                .expect("core data device")
                .get_data_device(&seat, &qh, ()),
            probe
                .primary
                .as_ref()
                .expect("primary selection")
                .get_device(&seat, &qh, ()),
            probe
                .wlr
                .as_ref()
                .expect("wlr data control")
                .get_data_device(&seat, &qh, ()),
            probe
                .ext
                .as_ref()
                .expect("ext data control")
                .get_data_device(&seat, &qh, ()),
        ));
    }
    queue.roundtrip(&mut probe).expect("all devices registered");
    println!("ready {count}");
    std::io::stdout().flush().unwrap();
    gate(directory, "act");
    match mode {
        "disconnect" | "legacy" => {}
        "release" | "seat-first" => {
            if mode == "seat-first" {
                seat.release();
                queue
                    .roundtrip(&mut probe)
                    .expect("seat released before devices");
            }
            for (core, primary, wlr, ext) in devices {
                core.release();
                primary.destroy();
                wlr.destroy();
                ext.destroy();
            }
            queue
                .roundtrip(&mut probe)
                .expect("all device destructors dispatched");
            println!("released {count}");
            std::io::stdout().flush().unwrap();
            gate(directory, "finish");
        }
        "killed" => panic!("parent must kill the waiting fixture, not release its gate"),
        _ => unreachable!(),
    }
}
