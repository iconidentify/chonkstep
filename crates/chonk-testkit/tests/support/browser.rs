//! Bounded DevTools observation of a private test browser. This helper never
//! sends CDP Input commands: mouse and keyboard events must cross the compositor.

use std::fs::File;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tungstenite::{client::client_with_config, protocol::WebSocketConfig, Message, WebSocket};

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_MESSAGE: usize = 1024 * 1024;

pub struct Browser {
    socket: WebSocket<TcpStream>,
    next_id: u64,
}

fn connection(port: u16) -> Result<TcpStream, String> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let stream =
        TcpStream::connect_timeout(&address, IO_TIMEOUT).map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(IO_TIMEOUT))
        .map_err(|error| error.to_string())?;
    Ok(stream)
}

fn targets(port: u16) -> Result<Value, String> {
    let mut stream = connection(port)?;
    write!(
        stream,
        "GET /json/list HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .map_err(|error| error.to_string())?;
    discovery_response(&mut stream)
}

fn discovery_response(stream: &mut impl Read) -> Result<Value, String> {
    // Chromium's small HTTP server can keep the socket open despite the
    // Connection: close request. The response ends at Content-Length, not EOF.
    let mut response = Vec::new();
    let mut buffer = [0; 4096];
    let header_end = loop {
        let count = stream
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if count == 0 {
            return Err("incomplete DevTools HTTP header".into());
        }
        response.extend_from_slice(&buffer[..count]);
        if let Some(end) = response.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            break end + 4;
        }
        if response.len() > 16 * 1024 {
            return Err("DevTools HTTP header exceeds the test's size limit".into());
        }
    };
    let header = std::str::from_utf8(&response[..header_end]).map_err(|error| error.to_string())?;
    if !header.starts_with("HTTP/1.1 200 ") {
        return Err(format!(
            "DevTools discovery failed: {}",
            header.lines().next().unwrap_or("")
        ));
    }
    let mut length = None;
    for line in header.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("Transfer-Encoding") {
            return Err("unexpected DevTools transfer encoding".into());
        }
        if name.eq_ignore_ascii_case("Content-Length") {
            if length.is_some() {
                return Err("duplicate DevTools Content-Length".into());
            }
            length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|error| error.to_string())?,
            );
        }
    }
    let length = length.ok_or("missing DevTools Content-Length")?;
    if length > MAX_MESSAGE {
        return Err("DevTools discovery exceeds the test's size limit".into());
    }
    let received = response.len() - header_end;
    if received < length {
        response.resize(header_end + length, 0);
        stream
            .read_exact(&mut response[header_end + received..])
            .map_err(|error| error.to_string())?;
    }
    serde_json::from_slice(&response[header_end..header_end + length])
        .map_err(|error| error.to_string())
}

impl Browser {
    /// The profile is newly created by this test, and the exact file URL has a
    /// unique per-case query. Never discover or attach to an ambient browser.
    pub fn connect(profile: &Path, expected_url: &str) -> Result<Self, String> {
        let mut port_file = String::new();
        File::open(profile.join("DevToolsActivePort"))
            .map_err(|error| error.to_string())?
            .take(4096)
            .read_to_string(&mut port_file)
            .map_err(|error| error.to_string())?;
        let port = port_file
            .lines()
            .next()
            .ok_or("empty DevTools port file")?
            .parse::<u16>()
            .map_err(|error| error.to_string())?;
        if port == 0 {
            return Err("DevTools did not announce a usable port".into());
        }
        let list = targets(port)?;
        let id = list
            .as_array()
            .ok_or("DevTools target list is not an array")?
            .iter()
            .find(|target| target["type"] == "page" && target["url"] == expected_url)
            .and_then(|target| target["id"].as_str())
            .ok_or("private test page not ready")?;
        if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("unexpected DevTools page identity".into());
        }
        let url = format!("ws://127.0.0.1:{port}/devtools/page/{id}");
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE))
            .max_frame_size(Some(MAX_MESSAGE));
        let (socket, _) = client_with_config(url, connection(port)?, Some(config))
            .map_err(|error| error.to_string())?;
        Ok(Self { socket, next_id: 1 })
    }

    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        assert!(
            !method.starts_with("Input."),
            "CDP input would bypass the compositor under test"
        );
        let id = self.next_id;
        self.next_id = id.checked_add(1).ok_or("DevTools request ID overflow")?;
        self.socket
            .send(Message::text(
                json!({"id":id, "method":method, "params":params}).to_string(),
            ))
            .map_err(|error| error.to_string())?;
        let deadline = Instant::now() + IO_TIMEOUT;
        for _ in 0..1024 {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or("DevTools response timed out")?;
            self.socket
                .get_mut()
                .set_read_timeout(Some(remaining))
                .map_err(|error| error.to_string())?;
            let message = self.socket.read().map_err(|error| error.to_string())?;
            if let Message::Text(text) = message {
                let value: Value =
                    serde_json::from_str(&text).map_err(|error| error.to_string())?;
                if value["id"].as_u64() != Some(id) {
                    continue;
                }
                if let Some(error) = value.get("error") {
                    return Err(format!("{method}: {error}"));
                }
                return value
                    .get("result")
                    .cloned()
                    .ok_or("missing DevTools result".into());
            }
        }
        Err("DevTools event stream exceeded the test's event limit".into())
    }

    pub fn evaluate(&mut self, expression: &str) -> Result<Value, String> {
        let value = self.call(
            "Runtime.evaluate",
            json!({
                "expression":expression, "returnByValue":true, "awaitPromise":true,
            "userGesture":true, "timeout":4000,
            }),
        )?;
        if let Some(error) = value.get("exceptionDetails") {
            return Err(format!("test page script: {error}"));
        }
        Ok(value["result"]["value"].clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct PersistentResponse(std::io::Cursor<Vec<u8>>);

    impl Read for PersistentResponse {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            let count = self.0.read(output)?;
            assert_ne!(count, 0, "discovery must not wait for the server to close");
            Ok(count)
        }
    }

    #[test]
    fn discovery_stops_at_content_length_on_a_persistent_connection() {
        let bytes = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n[]".to_vec();
        let mut stream = PersistentResponse(std::io::Cursor::new(bytes));
        assert_eq!(discovery_response(&mut stream).unwrap(), json!([]));
    }

    #[test]
    fn discovery_rejects_unbounded_or_ambiguous_responses() {
        for header in [
            "HTTP/1.1 200 OK\r\n",
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 2\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 2\r\n",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 2\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 1048577\r\n",
        ] {
            let mut stream = std::io::Cursor::new(format!("{header}\r\n[]"));
            assert!(discovery_response(&mut stream).is_err(), "{header}");
        }
    }

    #[test]
    fn discovery_requires_the_entire_declared_body() {
        let mut stream = std::io::Cursor::new(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\n[]");
        assert!(discovery_response(&mut stream).is_err());
    }
}
