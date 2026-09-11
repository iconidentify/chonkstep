//! Readback diagnostics from the running compositor, through its public control socket.
#![allow(clippy::disallowed_methods)]
use chonk_testkit::Session;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

pub fn counter(session: &Session, field: &str) -> u64 {
    let path = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap())
        .join("chonkstep").join(format!("control-{}.sock", session.wayland_display));
    let mut stream = UnixStream::connect(path).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    stream.write_all(b"{\"request\":\"debug\",\"topic\":\"scene\"}\n").unwrap();
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        let event: serde_json::Value = serde_json::from_str(&line).unwrap();
        if event["event"] != "debug" { continue; }
        let data = event["data"].as_str().unwrap();
        let line = data.lines().find(|line| line.starts_with("readback ")).unwrap();
        return line.split_whitespace().find_map(|word| word.strip_prefix(&format!("{field}=")))
            .unwrap().parse().unwrap();
    }
}
