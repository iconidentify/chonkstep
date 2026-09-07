//! A one-time damaged frame must finish a large capture fanout while input
//! remains responsive; newly requested damage-aware frames must still sleep.
use chonk_testkit::{poll_until, profile_binary, Session, SessionOptions};
use std::collections::HashSet;
use std::time::{Duration, Instant};

#[path = "support/screencopy.rs"]
mod screencopy;

#[test]
#[ignore = "needs nested Wayland; scripts/e2e.sh --headless --test screencopy_pressure"]
fn same_burst_buffer_cancellation_cannot_steal_a_plain_regions_presentation() {
    let mut session = Session::boot(
        "screencopy-cancel-burst",
        SessionOptions {
            config_extra: "show_dock = false\n".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let mut client = screencopy::Client::connect(&session);
    let canceled = client.region(20, 30, 16, 16);
    let plain = client.region(60, 30, 16, 16);
    let storage = client.storage(&canceled);
    let a = client.buffer(&storage, &canceled);
    let b = client.buffer(&storage, &plain);
    client.sync();
    session.door().barrier().unwrap();
    session.door().frame_stats().unwrap();

    // Flush all three requests in one native burst. Pruning when B is
    // admitted is too early: A's buffer is still alive at that point.
    canceled.resource.copy_with_damage(&a);
    plain.resource.copy(&b);
    a.destroy();
    client.sync();
    client.until("canceled buffer receives failure", |probe| {
        probe.failed.contains(&canceled.id)
    });
    println!(
        "canceled burst: plain_ready={} presented={}",
        client.probe.ready.contains(&plain.id),
        session.door().frame_stats().unwrap().render_calls,
    );
    // Native reads/syncs do not manufacture another scene change. The plain
    // request must finish on its own after the canceled geometry is removed.
    client.until_for(
        Duration::from_secs(2),
        "plain region after same-burst cancellation",
        |probe| probe.ready.contains(&plain.id),
    );
    assert!(!client.probe.failed.contains(&plain.id));
    canceled.resource.destroy();
    plain.resource.destroy();
    b.destroy();
    storage.pool.destroy();
    client.sync();
    assert!(session.compositor_alive());
}

#[test]
#[ignore = "needs nested Wayland; scripts/e2e.sh --headless --test screencopy_pressure"]
fn admitted_cancellation_preserves_later_plain_progress_during_fanout() {
    for destroy_frame in [false, true] {
        let mut session = Session::boot(
            if destroy_frame {
                "screencopy-cancel-frame"
            } else {
                "screencopy-cancel-admitted-buffer"
            },
            SessionOptions {
                config_extra: "show_dock = false\n".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let mut client = screencopy::Client::connect(&session);
        // An existing retained download keeps the newly admitted geometry
        // waiting long enough to observe its presentation without guessing a
        // four-millisecond scheduling race or changing product test hooks.
        let older: Vec<_> = (0..240).map(|_| client.region(20, 30, 16, 16)).collect();
        let canceled = client.region(60, 30, 16, 16);
        let plain = client.region(100, 30, 16, 16);
        let storage = client.storage(&older[0]);
        let buffers: Vec<_> = older
            .iter()
            .map(|frame| client.buffer(&storage, frame))
            .collect();
        let a = client.buffer(&storage, &canceled);
        let b = client.buffer(&storage, &plain);
        client.sync();
        session.door().barrier().unwrap();
        for (frame, buffer) in older.iter().zip(&buffers) {
            frame.resource.copy(buffer);
        }
        client.until("older retained fanout begins", |probe| {
            !probe.ready.is_empty()
        });
        assert!(client.probe.ready.len() < older.len());
        session.door().frame_stats().unwrap();
        canceled.resource.copy_with_damage(&a);
        plain.resource.copy(&b);
        client.sync();
        poll_until(
            Duration::from_secs(2),
            "waiting geometry has been presented",
            || (session.door().frame_stats().unwrap().render_calls > 0).then_some(()),
        )
        .unwrap();
        client.sync();
        assert!(
            client.probe.ready.len() < older.len(),
            "test must observe an active retained download"
        );
        assert!(!client.probe.ready.contains(&canceled.id));
        assert!(!client.probe.ready.contains(&plain.id));
        if destroy_frame {
            canceled.resource.destroy();
        } else {
            a.destroy();
        }
        client.sync();
        client.until_for(
            Duration::from_secs(3),
            "plain region after admitted cancellation",
            |probe| {
                probe.ready.contains(&plain.id)
                    && older.iter().all(|frame| probe.ready.contains(&frame.id))
            },
        );
        assert!(!client.probe.failed.contains(&plain.id));
        if destroy_frame {
            a.destroy();
        } else {
            assert!(client.probe.failed.contains(&canceled.id));
            canceled.resource.destroy();
        }
        for frame in older {
            frame.resource.destroy();
        }
        for buffer in buffers {
            buffer.destroy();
        }
        plain.resource.destroy();
        b.destroy();
        storage.pool.destroy();
        client.sync();
        assert!(session.compositor_alive());
    }
}

#[test]
#[ignore = "needs nested Wayland; scripts/e2e.sh --headless --test screencopy_pressure"]
fn dead_buffers_release_admission_without_a_presentation() {
    let mut session = Session::boot(
        "screencopy-dead-buffers",
        SessionOptions {
            config_extra: "show_dock = false\n".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let mut client = screencopy::Client::connect(&session);
    let frames: Vec<_> = (0..256).map(|_| client.region(20, 30, 16, 16)).collect();
    let storage = client.storage(&frames[0]);
    let buffers: Vec<_> = frames
        .iter()
        .map(|frame| client.buffer(&storage, frame))
        .collect();
    session.door().barrier().unwrap();
    for (frame, buffer) in frames.iter().zip(&buffers) {
        frame.resource.copy_with_damage(buffer);
    }
    client.sync();
    assert!(client.probe.ready.is_empty());
    assert!(client.probe.failed.is_empty());
    for buffer in buffers {
        buffer.destroy();
    }
    client.sync();
    client.until(
        "dead buffers fail without another admission or presentation",
        |probe| probe.failed.len() == 256,
    );
    assert!(client.probe.ready.is_empty());
    let replacement = client.region(20, 30, 16, 16);
    let replacement_buffer = client.buffer(&storage, &replacement);
    replacement.resource.copy(&replacement_buffer);
    client.until("replacement completes after dead-buffer pruning", |probe| {
        probe.ready.contains(&replacement.id)
    });
    assert_eq!(client.probe.failed.len(), 256);
    for frame in frames {
        frame.resource.destroy();
    }
    replacement.resource.destroy();
    replacement_buffer.destroy();
    storage.pool.destroy();
    client.sync();
}

#[test]
#[ignore = "needs nested Wayland; scripts/e2e.sh --headless --test screencopy_pressure"]
fn different_regions_keep_presentation_admission_and_plain_requests_make_progress() {
    let mut session = Session::boot(
        "screencopy-regions",
        SessionOptions {
            config_extra: "show_dock = false\n".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let mut client = screencopy::Client::connect(&session);
    let first = client.region(20, 30, 16, 16);
    let second = client.region(60, 30, 16, 16);
    let storage = client.storage(&first);
    let a = client.buffer(&storage, &first);
    let b = client.buffer(&storage, &second);
    session.door().barrier().unwrap();
    first.resource.copy_with_damage(&a);
    second.resource.copy_with_damage(&b);
    client.sync();
    session.door().motion(200.0, 200.0).unwrap();
    client.until("first region admitted", |probe| {
        probe.ready.contains(&first.id)
    });
    let started = Instant::now();
    poll_until(
        Duration::from_secs(2),
        "second region waits for its next presentation",
        || {
            client.sync();
            assert!(!client.probe.ready.contains(&second.id));
            (started.elapsed() >= Duration::from_millis(150)).then_some(())
        },
    )
    .unwrap();
    session.door().motion(201.0, 200.0).unwrap();
    client.until("next presentation admits second geometry", |probe| {
        probe.ready.contains(&second.id)
    });
    first.resource.destroy();
    second.resource.destroy();
    a.destroy();
    b.destroy();

    let plain: Vec<_> = (0..3)
        .map(|index| client.region(20 + index * 20, 60, 16, 16))
        .collect();
    let buffers: Vec<_> = plain
        .iter()
        .map(|frame| client.buffer(&storage, frame))
        .collect();
    for (frame, buffer) in plain.iter().zip(&buffers) {
        frame.resource.copy(buffer);
    }
    client.until(
        "all plain geometries complete without more input",
        |probe| plain.iter().all(|frame| probe.ready.contains(&frame.id)),
    );
    for frame in plain {
        frame.resource.destroy();
    }
    for buffer in buffers {
        buffer.destroy();
    }
    storage.pool.destroy();
    client.sync();
}

#[test]
#[ignore = "needs nested Wayland; scripts/e2e.sh --headless --test screencopy_pressure"]
fn locking_discards_an_unlocked_download_before_deferred_delivery() {
    let mut session = Session::boot(
        "screencopy-pressure-lock",
        SessionOptions {
            scale: Some(1.0),
            config_extra: "show_dock = false\n".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let mut client = screencopy::Client::connect(&session);
    let frames: Vec<_> = (0..256).map(|_| client.region(0, 0, 1280, 800)).collect();
    let storage = client.storage(&frames[0]);
    let buffers: Vec<_> = frames
        .iter()
        .map(|frame| client.buffer(&storage, frame))
        .collect();
    client.sync();
    for (frame, buffer) in frames.iter().zip(&buffers) {
        frame.resource.copy(buffer);
    }
    client.until("first unlocked result", |probe| !probe.ready.is_empty());
    assert!(client.probe.ready.len() < 256);
    let locker = profile_binary("chonk-lock-probe").unwrap();
    session
        .launch(locker.to_str().unwrap(), &["--hold"])
        .unwrap();
    poll_until(Duration::from_secs(5), "lock confirmation", || {
        session
            .client_log("chonk-lock-probe")
            .contains("locked ")
            .then_some(())
    })
    .unwrap();
    client.until("old download discarded", |probe| {
        probe.ready.len() + probe.failed.len() == 256
    });
    assert!(
        !client.probe.failed.is_empty(),
        "undelivered unlocked pixels must fail after locking"
    );

    let locked = session.screenshot("locked-reference").unwrap();
    let after = client.region(20, 30, 320, 200);
    let after_storage = client.storage(&after);
    let after_buffer = client.buffer(&after_storage, &after);
    after.resource.copy(&after_buffer);
    client.until("fresh locked capture", |probe| {
        probe.ready.contains(&after.id) || probe.failed.contains(&after.id)
    });
    assert!(client.probe.ready.contains(&after.id));
    let pixels = after.pixels(&after_storage);
    for y in 0..200 {
        for x in 0..320 {
            assert_eq!(pixels[(y * 320 + x) as usize], locked.pixel(x + 20, y + 30));
        }
    }
    for frame in frames {
        frame.resource.destroy();
    }
    for buffer in buffers {
        buffer.destroy();
    }
    after.resource.destroy();
    after_buffer.destroy();
    after_storage.pool.destroy();
    storage.pool.destroy();
    client.sync();
    assert!(session.compositor_alive());
}

#[test]
#[ignore = "needs nested Wayland; scripts/e2e.sh --headless --test screencopy_pressure"]
fn one_damage_fanout_yields_without_creating_a_self_sustaining_capture_loop() {
    let mut session = Session::boot(
        "screencopy-pressure",
        SessionOptions {
            scale: Some(1.0),
            config_extra: "desktop = \"omarchy\"\nomarchy_bar = false\nshow_dock = false\n".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let input = profile_binary("chonk-input-probe").unwrap();
    session.launch(input.to_str().unwrap(), &["1"]).unwrap();
    let window = session.wait_for_window("input-probe").unwrap();
    session
        .door()
        .motion((window.x + 30) as f64, (window.y + 50) as f64)
        .unwrap();
    session.door().barrier().unwrap();
    let reference = session.screenshot("before-fanout").unwrap();
    let releases = session
        .client_log("chonk-input-probe")
        .matches(" release ")
        .count();
    let mut client = screencopy::Client::connect(&session);
    let mut unrelated = screencopy::Client::connect(&session);
    let frames: Vec<_> = (0..257).map(|_| client.region(20, 30, 320, 200)).collect();
    let storage = client.storage(&frames[0]);
    // Distinct protocol buffers share one immutable-size allocation. The
    // compositor writes serially and the test only reads after every ready.
    // Keep first/middle/last destinations separate so the pixel oracle also
    // catches skipped writes hidden by aliasing the other consumers.
    let sentinels = [0, 127, 255].map(|index| (index, client.storage(&frames[index])));
    let buffers: Vec<_> = frames
        .iter()
        .enumerate()
        .map(|(index, frame)| {
            client.buffer(
                sentinels
                    .iter()
                    .find(|(at, _)| *at == index)
                    .map_or(&storage, |(_, storage)| storage),
                frame,
            )
        })
        .collect();
    client.sync();
    session.door().barrier().unwrap();
    session.door().frame_stats().unwrap();
    for (frame, buffer) in frames.iter().zip(&buffers) {
        frame.resource.copy_with_damage(buffer);
    }
    client.sync();
    assert_eq!(client.probe.failed, HashSet::from([frames[256].id]));
    assert!(
        client.probe.ready.is_empty(),
        "damage-aware admission does not request a frame"
    );
    assert_eq!(session.door().frame_stats().unwrap().render_calls, 0);

    // This is the one scene change admitting all 256 accepted requests.
    session
        .door()
        .motion((window.x + 31) as f64, (window.y + 50) as f64)
        .unwrap();
    client.until("first fanout result", |probe| !probe.ready.is_empty());
    let started = Instant::now();
    unrelated.sync();
    let roundtrip = started.elapsed();
    session.door().button("left", true).unwrap();
    session.door().button("left", false).unwrap();
    poll_until(
        Duration::from_secs(5),
        "button delivered during capture fanout",
        || {
            (session
                .client_log("chonk-input-probe")
                .matches(" release ")
                .count()
                == releases + 1)
                .then_some(())
        },
    )
    .unwrap();
    client.sync();
    assert!(
        client.probe.ready.len() < 256,
        "input must finish before the backlog"
    );
    println!(
        "wlr fanout roundtrip_us={} complete_at_input={}",
        roundtrip.as_micros(),
        client.probe.ready.len()
    );

    // A newcomer must not inherit the old cohort's eligibility or its pixels.
    let later = client.region(20, 30, 320, 200);
    let later_buffer = client.buffer(&storage, &later);
    later.resource.copy_with_damage(&later_buffer);
    client.sync();
    client.until("accepted fanout finishes without further damage", |probe| {
        probe.ready.len() >= 256
    });
    assert!(!client.probe.ready.contains(&later.id));
    assert!(!client.probe.failed.contains(&later.id));
    assert!(
        session.door().frame_stats().unwrap().render_calls <= 8,
        "draining copies must not force one presentation per batch"
    );
    for (index, storage) in &sentinels {
        let pixels = frames[*index].pixels(storage);
        for y in 0..200 {
            for x in 0..320 {
                assert_eq!(
                    pixels[(y * 320 + x) as usize],
                    reference.pixel(x + 20, y + 30)
                );
            }
        }
    }
    session
        .door()
        .motion((window.x + 32) as f64, (window.y + 50) as f64)
        .unwrap();
    client.until("new damage admits the newcomer", |probe| {
        probe.ready.contains(&later.id)
    });
    for frame in frames {
        frame.resource.destroy();
    }
    for buffer in buffers {
        buffer.destroy();
    }
    later.resource.destroy();
    later_buffer.destroy();
    storage.pool.destroy();
    for (_, storage) in sentinels {
        storage.pool.destroy();
    }
    client.sync();
    assert!(session.compositor_alive());
}
