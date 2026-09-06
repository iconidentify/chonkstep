//! A terminal child observing bytes produced by the real toolkit's repeat timer.
//! No timer or synthetic repeat lives here: the compositor injects a single
//! physical press and the terminal must produce the subsequent characters.

use std::io::{Read, Write};

fn main() {
    let path = std::env::args_os().nth(1).expect("private output path");
    let count: usize = std::env::args().nth(2).expect("byte count").parse().expect("positive byte count");
    assert!((1..=4096).contains(&count));
    let mut terminal = std::process::Command::new("stty")
        .args(["-icanon", "-echo", "min", "1", "time", "0"])
        .spawn()
        .expect("configure this test's private terminal");
    let wait = || terminal.try_wait().expect("terminal configuration status");
    let status = match chonk_testkit::poll_until(std::time::Duration::from_secs(5), "raw terminal configuration", wait) {
        Ok(status) => status,
        Err(error) => {
            let _ = terminal.kill();
            let _ = chonk_testkit::poll_until(std::time::Duration::from_secs(5), "terminal helper to exit", || {
                terminal.try_wait().ok().flatten()
            });
            panic!("{error}");
        }
    };
    assert!(status.success(), "raw terminal input");
    // Creation is the readiness signal and happens only after the terminal
    // accepts individual bytes without waiting for Return.
    let mut output =
        std::fs::OpenOptions::new().write(true).create_new(true).open(path).expect("exclusive test output");
    let mut input = std::io::stdin().lock();
    for _ in 0..count {
        let mut byte = [0];
        input.read_exact(&mut byte).expect("terminal input byte");
        output.write_all(&byte).expect("capture terminal byte");
    }
}
