use super::*;

fn live_child() -> Child {
    Command::new("/usr/bin/sleep")
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

#[test]
fn clipboard_candidates_are_serialized_and_admission_stays_charged_until_resolution() {
    let mut clipboard = Clipboard {
        provider: Some(live_child()),
        candidate: None,
        waiting: VecDeque::new(),
    };
    let old_provider = clipboard.provider.as_ref().unwrap().id();
    let first = PathBuf::from("/published/first.png");
    let second = PathBuf::from("/published/second.png");
    let third = PathBuf::from("/published/third.png");
    clipboard.enqueue(first.clone()).unwrap();
    clipboard.enqueue(second.clone()).unwrap();
    assert_eq!(clipboard.enqueue(third.clone()), Err(third.clone()));
    let mut launched = Vec::new();
    let completed = clipboard.poll(Instant::now(), |path| {
        launched.push(path.to_owned());
        Ok(live_child())
    });
    assert!(completed.is_empty());
    assert_eq!(launched.as_slice(), std::slice::from_ref(&first));
    let first_pid = clipboard.candidate.as_ref().unwrap().child.id();
    let deadline = clipboard.deadline().unwrap();
    // Use the actual candidate deadline with synthetic earlier instants.
    // The child stays alive for 30 seconds; a blocking liveness probe would
    // prevent this independent queue work from completing immediately.
    for before_ms in [99, 50, 1] {
        assert!(clipboard
            .poll(deadline - Duration::from_millis(before_ms), |_| {
                panic!("a second provider started before the first was resolved")
            })
            .is_empty());
        assert_eq!(clipboard.provider.as_ref().unwrap().id(), old_provider);
        assert_eq!(clipboard.enqueue(third.clone()), Err(third.clone()));
    }
    let completed = clipboard.poll(deadline, |path| {
        launched.push(path.to_owned());
        Ok(live_child())
    });
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].path, first);
    assert!(completed[0].copied);
    assert_eq!(clipboard.provider.as_ref().unwrap().id(), first_pid);
    assert_eq!(launched, [first, second.clone()]);
    assert_eq!(clipboard.candidate.as_ref().unwrap().path, second);
    clipboard.enqueue(third).unwrap();
}

#[test]
fn clipboard_spawn_failure_preserves_the_previous_provider_and_drains_failed_paths() {
    let mut clipboard = Clipboard {
        provider: Some(live_child()),
        candidate: None,
        waiting: VecDeque::new(),
    };
    let previous = clipboard.provider.as_ref().unwrap().id();
    clipboard
        .enqueue(PathBuf::from("/published/first.png"))
        .unwrap();
    clipboard
        .enqueue(PathBuf::from("/published/second.png"))
        .unwrap();
    let completed = clipboard.poll(Instant::now(), |_| {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "isolated missing wl-copy",
        ))
    });
    assert_eq!(completed.len(), 2);
    assert!(completed.iter().all(|result| !result.copied));
    assert_eq!(completed[0].path, Path::new("/published/first.png"));
    assert_eq!(completed[1].path, Path::new("/published/second.png"));
    assert_eq!(clipboard.provider.as_ref().unwrap().id(), previous);
    assert!(clipboard
        .provider
        .as_mut()
        .unwrap()
        .try_wait()
        .unwrap()
        .is_none());
    assert!(clipboard.waiting.is_empty());
    assert_eq!(clipboard.deadline(), None);
}

#[test]
fn early_clipboard_exit_reports_its_real_status_and_preserves_the_previous_provider() {
    for code in [0, 17] {
        let mut clipboard = Clipboard {
            provider: Some(live_child()),
            candidate: None,
            waiting: VecDeque::new(),
        };
        let previous = clipboard.provider.as_ref().unwrap().id();
        clipboard
            .enqueue(PathBuf::from("/published/capture.png"))
            .unwrap();
        assert!(clipboard
            .poll(Instant::now(), |_| {
                Command::new("/bin/sh")
                    .arg("-c")
                    .arg(format!("exit {code}"))
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
            })
            .is_empty());
        // Force this test to observe the exit, even on a heavily delayed CI
        // process; the normal liveness deadline is tested separately above.
        clipboard.candidate.as_mut().unwrap().deadline = Instant::now() + Duration::from_secs(60);
        let timeout = Instant::now() + Duration::from_secs(3);
        let completed = loop {
            let completed = clipboard.poll(Instant::now(), |_| panic!("unexpected extra launch"));
            if !completed.is_empty() {
                break completed;
            }
            assert!(Instant::now() < timeout, "fixture helper did not exit");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].copied, code == 0);
        assert_eq!(clipboard.provider.as_ref().unwrap().id(), previous);
        assert!(clipboard
            .provider
            .as_mut()
            .unwrap()
            .try_wait()
            .unwrap()
            .is_none());
        assert_eq!(clipboard.deadline(), None);
    }
}

#[test]
fn clipboard_shutdown_retires_only_clipboard_helpers_and_empty_state_never_wakes() {
    let mut clipboard = Clipboard::default();
    let mut background = Background::default();
    assert_eq!(clipboard.deadline(), None);
    assert_eq!(
        background.interval(false, clipboard.provider.is_some()),
        None
    );
    clipboard.provider = Some(live_child());
    clipboard
        .enqueue(PathBuf::from("/published/current.png"))
        .unwrap();
    clipboard
        .enqueue(PathBuf::from("/published/waiting.png"))
        .unwrap();
    assert!(clipboard
        .poll(Instant::now(), |_| Ok(live_child()))
        .is_empty());
    let mut review_command = Command::new("/usr/bin/sleep");
    review_command
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    background
        .launch_review(
            ReviewKind::Screenshot,
            PathBuf::from("/published/review.png"),
            &mut review_command,
        )
        .unwrap();
    clipboard.shutdown();
    clipboard.shutdown(); // Teardown also runs from Drop.
    assert!(
        clipboard.provider.is_none()
            && clipboard.candidate.is_none()
            && clipboard.waiting.is_empty()
    );
    assert_eq!(clipboard.deadline(), None);
    assert!(
        background.reviews[0].child.try_wait().unwrap().is_none(),
        "user review windows survive clipboard shutdown"
    );
    let mut review = background.reviews.pop().unwrap();
    review.child.kill().unwrap();
    review.child.wait().unwrap();
    assert_eq!(
        background.interval(false, clipboard.provider.is_some()),
        None
    );
}
