use serde_json::{json, Value};
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const HELPER: &str = env!("CARGO_BIN_EXE_chonk-agent-hook");
static NEXT: AtomicU32 = AtomicU32::new(0);
// Avoid another test forking while a fixture executable is open for copying.
static NATIVE: Mutex<()> = Mutex::new(());

struct Fixture {
    path: PathBuf,
    listener: OwnedFd,
}

impl Fixture {
    fn new(engine: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "chonk-hook-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let agents = path.join("chonk-agents");
        std::fs::create_dir(&agents).unwrap();
        std::fs::set_permissions(&agents, std::fs::Permissions::from_mode(0o700)).unwrap();
        // A harmless shell stands in for a native runtime. No provider request or
        // provider configuration is involved; /proc/exe has a real native basename.
        std::fs::copy("/bin/sh", path.join(engine)).unwrap();
        let raw = unsafe {
            libc::socket(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                0,
            )
        };
        assert!(raw >= 0);
        let listener = unsafe { OwnedFd::from_raw_fd(raw) };
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        address.sun_family = libc::AF_UNIX as _;
        for (target, source) in address
            .sun_path
            .iter_mut()
            .zip(agents.join("events.sock").as_os_str().as_bytes())
        {
            *target = *source as _;
        }
        assert_eq!(
            unsafe {
                libc::bind(
                    raw,
                    (&address as *const libc::sockaddr_un).cast(),
                    std::mem::size_of_val(&address) as _,
                )
            },
            0
        );
        assert_eq!(unsafe { libc::listen(raw, 8) }, 0);
        Self { path, listener }
    }

    fn command(&self, engine: &str, mode: &str) -> Command {
        let mut command = Command::new(self.path.join(engine));
        // The final ':' prevents the shell from exec-replacing the owning process.
        command.args(["-c", "\"$1\" \"$2\"; :", "fixture", HELPER, mode]);
        command
            .env("XDG_RUNTIME_DIR", &self.path)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn receive(&self) -> Value {
        // The helper has already exited: it cannot be waiting for a reply here.
        let raw = unsafe {
            libc::accept4(
                self.listener.as_raw_fd(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            )
        };
        assert!(
            raw >= 0,
            "expected queued connection: {}",
            std::io::Error::last_os_error()
        );
        let socket = unsafe { OwnedFd::from_raw_fd(raw) };
        let mut bytes = [0u8; 16384];
        let len = unsafe {
            libc::recv(
                socket.as_raw_fd(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
                0,
            )
        };
        assert!(len > 0);
        serde_json::from_slice(&bytes[..len as usize]).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn actual_seqpacket_enqueue_does_not_wait_for_a_reply_and_has_owning_pid() {
    let _serial = NATIVE.lock().unwrap();
    let fixture = Fixture::new("codex");
    let mut child = fixture.command("codex", "codex").spawn().unwrap();
    let pid = child.id();
    let input = json!({"session_id":"native", "turn_id":"turn", "hook_event_name":"Stop", "cwd":"/work/project", "last_assistant_message":"PRIVATE"});
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"{}\n");
    assert!(output.stderr.is_empty());
    let payload = fixture.receive();
    assert_eq!(payload["op"], "session");
    assert_eq!(payload["event"]["pid"], pid);
    assert!(payload["event"]["start_time"].as_u64().unwrap() > 0);
    assert!(payload["event"]["observed_ns"].as_u64().unwrap() > 0);
    assert_eq!(payload["event"]["kind"], "turn.stopped");
    assert!(!payload.to_string().contains("PRIVATE"));
}

#[test]
fn inherited_tmux_binding_is_bounded_and_claude_stdout_is_empty() {
    let _serial = NATIVE.lock().unwrap();
    let fixture = Fixture::new("claude");
    let mut child = fixture
        .command("claude", "claude")
        .env("TMUX", "/tmp/with,comma/socket,123,0")
        .env("TMUX_PANE", "%8")
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"session_id":"s","prompt_id":"p","hook_event_name":"Stop"}"#)
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    let event = fixture.receive()["event"].clone();
    assert_eq!(event["turn_id"], "p");
    assert_eq!(event["terminal"]["tmux_socket"], "/tmp/with,comma/socket");
    assert_eq!(event["terminal"]["tmux_pane"], "%8");
}

#[test]
fn truncated_or_oversized_input_fails_open_without_partial_events() {
    let _serial = NATIVE.lock().unwrap();
    let fixture = Fixture::new("codex");
    let mut oversized = br#"{"session_id":"s","hook_event_name":"Stop"}"#.to_vec();
    oversized.resize(chonk_agent_hook::INPUT_LIMIT + 1, b' ');
    for bytes in [b"{\"session_id\":".to_vec(), oversized] {
        let mut child = fixture.command("codex", "codex").spawn().unwrap();
        let _ = child.stdin.take().unwrap().write_all(&bytes);
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
    let mut poll = libc::pollfd {
        fd: fixture.listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(unsafe { libc::poll(&mut poll, 1, 0) }, 0);
}

#[test]
fn unfinished_input_pipe_has_a_hard_deadline_and_exit_zero() {
    let _serial = NATIVE.lock().unwrap();
    let start = Instant::now();
    let mut command = Command::new(HELPER);
    command
        .arg("codex")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        command.pre_exec(|| {
            let mut signals: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut signals);
            libc::sigaddset(&mut signals, libc::SIGALRM);
            if libc::sigprocmask(libc::SIG_BLOCK, &signals, std::ptr::null_mut()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    let mut writer = child.stdin.take().unwrap();
    writer.write_all(b"{").unwrap();
    // Keep the pipe open while waiting: EOF will never arrive.
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert!(start.elapsed() < Duration::from_millis(500));
}

#[test]
fn valid_input_without_native_owner_emits_nothing() {
    let _serial = NATIVE.lock().unwrap();
    let fixture = Fixture::new("observer");
    // argv[0] cannot forge ownership: the real executable is this integration test.
    let mut child = Command::new(HELPER)
        .arg("claude")
        .env("XDG_RUNTIME_DIR", &fixture.path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"session_id":"s","hook_event_name":"SessionStart"}"#)
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    let mut poll = libc::pollfd {
        fd: fixture.listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(unsafe { libc::poll(&mut poll, 1, 0) }, 0);
}

#[test]
fn codex_notify_reads_one_argument_without_waiting_for_stdin() {
    let _serial = NATIVE.lock().unwrap();
    let fixture = Fixture::new("codex");
    let payload = json!({"type":"agent-turn-complete", "thread-id":"native", "turn-id":"turn", "last-assistant-message":"PRIVATE"}).to_string();
    let mut child = Command::new(fixture.path.join("codex"))
        .args([
            "-c",
            "\"$1\" codex-notify \"$2\"; :",
            "fixture",
            HELPER,
            &payload,
        ])
        .env("XDG_RUNTIME_DIR", &fixture.path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let _open_stdin = child.stdin.take().unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    let event = fixture.receive()["event"].clone();
    assert_eq!(event["kind"], "turn.complete");
    assert_eq!(event["session_id"], "native");
    assert!(!event.to_string().contains("PRIVATE"));
}
