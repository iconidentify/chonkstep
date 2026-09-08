//! Connect once using the DISPLAY inherited at exec, then map a plain X11
//! window. Used to verify that X11 autostart gets this compositor's server
//! before its first event-loop iteration, including in nested sessions.

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, CreateWindowAux, EventMask, WindowClass};
use x11rb::wrapper::ConnectionExt as _;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let marker = std::env::args().nth(1).ok_or("expected marker path")?;
    if std::env::args().nth(2).as_deref() == Some("check-scale-env") {
        for name in ["GDK_SCALE", "GDK_DPI_SCALE"] {
            if std::env::var_os(name).is_some() {
                return Err(format!("inherited {name} would double-apply desktop scaling").into());
            }
        }
    }
    let display = std::env::var("DISPLAY")?;
    let (connection, screen_number) = x11rb::connect(None)?;
    let screen = &connection.setup().roots[screen_number];
    let window = connection.generate_id()?;
    connection.create_window(
        x11rb::COPY_DEPTH_FROM_PARENT, window, screen.root, 0, 0, 400, 200, 0,
        WindowClass::INPUT_OUTPUT, x11rb::COPY_FROM_PARENT,
        &CreateWindowAux::new().background_pixel(0x406080).event_mask(EventMask::STRUCTURE_NOTIFY),
    )?;
    connection.change_property8(
        x11rb::protocol::xproto::PropMode::REPLACE, window, AtomEnum::WM_NAME,
        AtomEnum::STRING, b"X11 autostart probe",
    )?;
    connection.change_property8(
        x11rb::protocol::xproto::PropMode::REPLACE, window, AtomEnum::WM_CLASS,
        AtomEnum::STRING, b"x11-autostart\0X11Autostart\0",
    )?;
    connection.map_window(window)?;
    connection.flush()?;
    std::fs::write(marker, format!("DISPLAY={display}\n"))?;
    // This is the probe client, not the compositor: wait for its server
    // to disconnect, keeping the mapped window alive for the assertion.
    while connection.wait_for_event().is_ok() {}
    Ok(())
}
