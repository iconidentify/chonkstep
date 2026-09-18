//! A bounded, nonblocking subscriber to the existing desktop control socket.
use chonk_ipc::Stream;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::os::fd::{AsRawFd, RawFd};
use std::time::{Duration, Instant};

pub struct Control {
    display: String,
    stream: Option<Stream>,
    retry: Instant,
    input: Vec<u8>,
    output: VecDeque<u8>,
    pub current: usize,
    pub count: usize,
    pub theme: Option<(String, wm_theme::Appearance)>,
}
impl Default for Control {
    fn default() -> Self {
        Self {
            display: crate::client::Platform::detect().display_name(),
            stream: None,
            retry: Instant::now(),
            input: Vec::new(),
            output: VecDeque::new(),
            current: 0,
            count: 1,
            theme: None,
        }
    }
}
impl Control {
    pub fn for_display(display: String) -> Self {
        Self {
            display,
            ..Self::default()
        }
    }
    pub fn fd(&self) -> Option<RawFd> {
        self.stream.as_ref().map(AsRawFd::as_raw_fd)
    }
    pub fn wants_write(&self) -> bool {
        !self.output.is_empty()
    }
    pub fn connected(&self) -> bool {
        self.stream.is_some()
    }
    pub fn focus_workspace(&mut self, index: usize) {
        if self.connected() && index < self.count && self.output.len() < 4096 {
            self.output.extend(
                format!("{}\n", json!({"request":"focus-workspace","index":index})).bytes(),
            );
        }
    }
    fn disconnect(&mut self) {
        self.stream = None;
        self.input.clear();
        self.output.clear();
        self.retry = Instant::now() + Duration::from_secs(2);
    }
    pub fn tick(&mut self) {
        if self.stream.is_none() && Instant::now() >= self.retry {
            let path = std::env::var_os("CHONKSTEP_CONTROL_SOCKET")
                .map(std::path::PathBuf::from)
                .or_else(|| chonk_ipc::control_socket_path(&self.display).ok());
            self.stream = path.and_then(|path| Stream::connect(&path).ok());
            self.retry = Instant::now() + Duration::from_secs(2);
        }
        let Some(stream) = &self.stream else { return };
        if !self.output.is_empty() {
            match stream.send(self.output.make_contiguous()) {
                Ok(n) => {
                    self.output.drain(..n);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => {
                    self.disconnect();
                    return;
                }
            }
        }
        let mut bytes = [0u8; 8192];
        // Bound both bytes and events per dispatch; no server can monopolize
        // the dock loop or grow a partial JSON line without limit.
        for _ in 0..8 {
            match stream.recv(&mut bytes) {
                Ok(0) => {
                    self.disconnect();
                    return;
                }
                Ok(n) => self.input.extend_from_slice(&bytes[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    self.disconnect();
                    return;
                }
            }
        }
        let mut consumed = 0;
        for line in self.input.split_inclusive(|b| *b == b'\n').take(64) {
            if line.last() != Some(&b'\n') {
                break;
            }
            consumed += line.len();
            let Ok(event) = serde_json::from_slice::<Value>(line) else {
                continue;
            };
            match event["event"].as_str() {
                Some("workspaces") => {
                    self.count = event["workspaces"]
                        .as_array()
                        .map_or(1, |v| v.len().clamp(1, 4096));
                    self.current =
                        (event["active"].as_u64().unwrap_or(0) as usize).min(self.count - 1);
                }
                Some("theme") => {
                    if let (Some(id), Some(mode)) = (
                        event["id"].as_str(),
                        event["appearance"]
                            .as_str()
                            .and_then(wm_theme::Appearance::from_name),
                    ) {
                        if id.len() <= 128 {
                            self.theme = Some((id.to_string(), mode));
                        }
                    }
                }
                _ => {}
            }
        }
        self.input.drain(..consumed);
        if self.input.len() > 65_536 {
            self.disconnect();
        }
    }
}
