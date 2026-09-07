//! Real screencopy storage churn, idle retirement and subsequent pixel reuse.
use chonk_testkit::{poll_until, Session, SessionOptions};
use std::time::Duration;

#[path = "support/screencopy.rs"]
mod screencopy;

fn region(client: &mut screencopy::Client, x: u32, y: u32, w: u32, h: u32) -> Vec<[u8; 4]> {
    let frame = client.region(x as i32, y as i32, w as i32, h as i32);
    assert_eq!(frame.size, (w, h));
    let storage = client.storage(&frame);
    let buffer = client.buffer(&storage, &frame);
    frame.resource.copy(&buffer);
    client.until("region capture completes", |probe| {
        probe.ready.contains(&frame.id) || probe.failed.contains(&frame.id)
    });
    assert!(client.probe.ready.contains(&frame.id));
    let pixels = frame.pixels(&storage);
    frame.resource.destroy();
    buffer.destroy();
    storage.pool.destroy();
    client.sync();
    pixels
}

#[test]
#[ignore = "needs nested Wayland; scripts/e2e.sh --headless --test capture_cache"]
fn region_churn_and_idle_retirement_preserve_authoritative_pixels() {
    let mut session = Session::boot(
        "capture-cache",
        SessionOptions {
            config_extra: "show_dock = false\n".into(),
            ..SessionOptions::default()
        },
    )
    .unwrap();
    let full = session.screenshot("reference").unwrap();
    let mut client = screencopy::Client::connect(&session);
    for index in 0..14 {
        let (x, y, w, h) = (20 + index, 30 + index, 400 - index, 260 + index);
        let capture = region(&mut client, x, y, w, h);
        for cy in 0..h {
            for cx in 0..w {
                assert_eq!(capture[(cy * w + cx) as usize], full.pixel(x + cx, y + cy));
            }
        }
        let (entries, bytes) = session.door().capture_cache().unwrap();
        assert!(entries > 0 && entries <= 8);
        assert!(bytes <= 64 * 1024 * 1024);
    }
    poll_until(
        Duration::from_secs(10),
        "idle capture storage is retired without another screenshot",
        || (session.door().capture_cache().unwrap() == (0, 0)).then_some(()),
    )
    .unwrap();
    let after = region(&mut client, 20, 30, 400, 260);
    for y in 0..260 {
        for x in 0..400 {
            assert_eq!(after[(y * 400 + x) as usize], full.pixel(x + 20, y + 30));
        }
    }
    assert_eq!(session.door().capture_cache().unwrap(), (1, 400 * 260 * 4));
}
