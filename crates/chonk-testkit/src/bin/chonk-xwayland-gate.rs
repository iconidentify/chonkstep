//! Test-only PATH shim: hold XWayland startup until the private fixture has
//! published its native selections. No compositor delay hook is shipped.

use std::os::unix::process::CommandExt;
use std::process::Command;
use std::time::{Duration, Instant};

fn main() {
    let path = std::env::var_os("PATH").expect("private shim PATH");
    let directory = std::env::split_paths(&path).next().expect("shim directory");
    assert!(directory
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("chonk-xwayland-gate-"));
    std::fs::write(directory.join("started"), b"ready").unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !directory.join("release").exists() {
        assert!(
            Instant::now() < deadline,
            "XWayland startup fixture did not release its gate"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let error = Command::new(directory.join("real-Xwayland"))
        .args(std::env::args_os().skip(1))
        .exec();
    panic!("exec private XWayland: {error}");
}
