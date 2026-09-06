//! Optional slow/gated consumers for exercising real bridge backpressure and
//! cancellation. Gates are private fixture files, never compositor controls.

use std::io::Read;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Default)]
pub(super) enum Mode {
    #[default]
    Immediate,
    Slow,
    Gated,
}

impl Mode {
    pub(super) fn parse(value: Option<&str>) -> Self {
        match value {
            None => Self::Immediate,
            Some("slow") => Self::Slow,
            Some("gated") => Self::Gated,
            Some(other) => panic!("unknown receive mode {other}"),
        }
    }
}

pub(super) fn bytes(
    mut reader: UnixStream,
    mode: Mode,
    directory: &Path,
    sequence: usize,
    kind: &str,
) -> std::io::Result<Option<Vec<u8>>> {
    if matches!(mode, Mode::Gated) {
        let resume = directory.join(format!("receive-{sequence}.resume"));
        let cancel = directory.join(format!("receive-{sequence}.cancel"));
        let deadline = Instant::now() + Duration::from_secs(10);
        super::say(format!("receive ready {kind} {sequence}"));
        while !resume.exists() {
            if cancel.exists() {
                super::say(format!("receive cancelled {kind} {sequence}"));
                return Ok(None);
            }
            if Instant::now() >= deadline {
                return Err(std::io::ErrorKind::TimedOut.into());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    let mut bytes = Vec::new();
    if matches!(mode, Mode::Immediate) {
        reader
            .take((super::LIMIT + 1) as u64)
            .read_to_end(&mut bytes)?;
    } else {
        let mut chunk = [0; 1733];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => {
                    bytes.extend_from_slice(&chunk[..count]);
                    assert!(bytes.len() <= super::LIMIT, "bounded test payload");
                    // Deliberate consumer rate, not a test-completion sleep.
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
    }
    assert!(bytes.len() <= super::LIMIT, "bounded test payload");
    Ok(Some(bytes))
}
