//! Registry-level acceptance test for the standard desktop protocols.
//!
//! `wayland-info` is a real generic client: it binds every advertised
//! global at the announced version and walks its objects. That catches
//! both a missing registration and a dispatch/delegation mismatch that
//! a unit test over a state constructor cannot see.

use std::time::Duration;

use chonk_testkit::{poll_until, Session, SessionOptions};

#[test]
#[ignore = "needs a live Wayland session and wayland-info"]
// This ignored integration test runs on Cargo's test thread, never the
// compositor repaint thread. The synchronous probe is only an
// availability check before the real client is supervised by Session.
#[allow(clippy::disallowed_methods)]
fn ordinary_desktop_globals_bind_successfully() {
    if std::process::Command::new("wayland-info")
        .arg("--help")
        .output()
        .is_err()
    {
        eprintln!("SKIP: wayland-info is not installed");
        return;
    }
    let mut session = Session::boot("wayland-globals", SessionOptions::default()).unwrap();
    session
        .launch("wayland-info", &[])
        .expect("wayland-info launches");
    let report = poll_until(
        Duration::from_secs(10),
        "wayland-info to enumerate the registry",
        || {
            let report = session.client_log("wayland-info");
            report
                .contains("interface: 'xdg_wm_base'")
                .then_some(report)
        },
    )
    .expect("wayland-info reaches the end of the registry walk");

    for interface in [
        "xdg_activation_v1",
        "wp_cursor_shape_manager_v1",
        "wp_single_pixel_buffer_manager_v1",
        "wp_presentation",
        "zwp_relative_pointer_manager_v1",
        "zwp_pointer_constraints_v1",
        "zwp_pointer_gestures_v1",
        "zwp_tablet_manager_v2",
        "zxdg_exporter_v2",
        "zxdg_importer_v2",
        "zwp_keyboard_shortcuts_inhibit_manager_v1",
        "zwp_text_input_manager_v3",
        "zwp_input_method_manager_v2",
        "xdg_wm_dialog_v1",
        "xdg_system_bell_v1",
        "xdg_toplevel_tag_manager_v1",
        "hyprland_toplevel_mapping_manager_v1",
    ] {
        assert!(
            report.contains(&format!("interface: '{interface}'")),
            "missing {interface}\n{report}"
        );
    }
    assert!(
        session.compositor_alive(),
        "binding and walking every global must not terminate the compositor"
    );
}
