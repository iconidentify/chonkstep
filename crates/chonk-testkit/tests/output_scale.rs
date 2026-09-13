//! Screen geometry seen by a real bar client must match layer-shell units.
use std::time::Duration;
use chonk_testkit::{poll_until, Session, SessionOptions};

#[test]
#[ignore = "needs a live Wayland session and wayland-info"]
fn fractional_output_reports_full_logical_screen() {
    if !chonk_testkit::require_client("wayland-info") { return; }
    let mut session = Session::boot("fractional-output-geometry", SessionOptions {
        scale: Some(1.5), ..Default::default()
    }).unwrap();
    let world = session.world().unwrap();
    session.launch("wayland-info", &[]).unwrap();
    let status = poll_until(Duration::from_secs(10), "wayland-info to exit", || {
        session.client_status("wayland-info").ok().flatten()
    }).unwrap();
    let report = session.client_log("wayland-info");
    assert!(status.success(), "{report}");
    let expected = format!("logical_width: {}, logical_height: {}",
        (f64::from(world.output_w) / 1.5).round() as i32,
        (f64::from(world.output_h) / 1.5).round() as i32);
    assert!(report.contains(&expected), "missing {expected}\n{report}");
}
