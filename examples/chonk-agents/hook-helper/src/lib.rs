//! Bounded, metadata-only Linux hook observer. No provider requests or history reads.

use serde_json::{json, Map, Value};
use std::ffi::OsStr;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const INPUT_LIMIT: usize = 1024 * 1024;
const DEADLINE_MS: u64 = 100;
const EVENT_LIMIT: usize = 16 * 1024;

extern "C" fn expired(_: libc::c_int) {
    // _exit is async-signal-safe; never let a full pipe or unavailable service hang a hook.
    unsafe { libc::_exit(0) }
}

pub fn install_deadline() {
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = expired as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        if libc::sigaction(libc::SIGALRM, &action, std::ptr::null_mut()) != 0 {
            libc::_exit(0);
        }
        let mut signals: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut signals);
        libc::sigaddset(&mut signals, libc::SIGALRM);
        if libc::sigprocmask(libc::SIG_UNBLOCK, &signals, std::ptr::null_mut()) != 0 {
            libc::_exit(0);
        }
        let timer = libc::itimerval {
            it_interval: libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            it_value: libc::timeval {
                tv_sec: 0,
                tv_usec: (DEADLINE_MS * 1000) as _,
            },
        };
        if libc::setitimer(libc::ITIMER_REAL, &timer, std::ptr::null_mut()) != 0 {
            libc::_exit(0);
        }
    }
}

pub fn run() {
    let observed_ns = monotonic_ns();
    let deadline = Instant::now() + Duration::from_millis(DEADLINE_MS);
    let mut args = std::env::args_os().skip(1);
    let Some(mode) = args.next() else { return };
    let notify = mode == "codex-notify";
    let engine = match mode.to_str() {
        Some("codex" | "codex-notify") => "codex",
        Some("claude") => "claude",
        _ => return,
    };
    let input = if notify {
        let Some(payload) = args.next() else { return };
        if args.next().is_some() || payload.as_bytes().len() > INPUT_LIMIT {
            return;
        }
        payload.as_bytes().to_vec()
    } else {
        if args.next().is_some() {
            return;
        }
        let Ok(input) = read_input(libc::STDIN_FILENO, deadline) else {
            return;
        };
        input
    };
    let Ok(payload) = serde_json::from_slice::<Value>(&input) else {
        return;
    };
    // JSON {} is a neutral structured result for Codex Stop/SubagentStop. Emit it
    // before all observation I/O; no diagnostics, broker response, or context leaks.
    if engine == "codex"
        && matches!(
            payload.get("hook_event_name").and_then(Value::as_str),
            Some("Stop" | "SubagentStop")
        )
    {
        unsafe {
            libc::write(libc::STDOUT_FILENO, b"{}\n".as_ptr().cast(), 3);
        }
    }
    let Some(mut event) = map_event(engine, notify, &payload) else {
        return;
    };
    let Some(owner) = find_owner(engine) else {
        return;
    };
    event.insert("observed_ns".into(), observed_ns.into());
    event.insert("pid".into(), owner.pid.into());
    event.insert("start_time".into(), owner.start_time.into());
    let terminal = terminal_metadata(&owner);
    if !terminal.is_empty() {
        event.insert("terminal".into(), Value::Object(terminal));
    }
    let Ok(bytes) = serde_json::to_vec(&json!({"op":"session", "event":event})) else {
        return;
    };
    if bytes.len() > EVENT_LIMIT || Instant::now() >= deadline {
        return;
    }
    let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") else {
        return;
    };
    let _ = send_event(Path::new(&runtime), &bytes);
}

fn monotonic_ns() -> u64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) } != 0 {
        return 0;
    }
    (time.tv_sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(time.tv_nsec as u64)
}

fn read_input(fd: RawFd, deadline: Instant) -> io::Result<Vec<u8>> {
    let mut input = Vec::with_capacity(4096);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut poll, 1, remaining.as_millis().max(1) as i32) };
        if ready <= 0 {
            if ready < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(io::ErrorKind::TimedOut.into());
        }
        let mut chunk = [0u8; 8192];
        // The extra byte detects oversized input instead of parsing a valid prefix.
        let count = chunk.len().min(INPUT_LIMIT + 1 - input.len());
        let len = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), count) };
        if len == 0 {
            return Ok(input);
        }
        if len < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(io::Error::last_os_error());
        }
        input.extend_from_slice(&chunk[..len as usize]);
        if input.len() > INPUT_LIMIT {
            return Err(io::ErrorKind::InvalidData.into());
        }
    }
}

fn field<'a>(input: &'a Value, name: &str, max: usize) -> Option<&'a str> {
    let text = input.get(name)?.as_str()?;
    (!text.is_empty() && text.len() <= max && !text.chars().any(char::is_control)).then_some(text)
}

fn copy_field(
    output: &mut Map<String, Value>,
    name: &str,
    input: &Value,
    source: &str,
    max: usize,
) {
    if let Some(value) = field(input, source, max) {
        output.insert(name.into(), value.into());
    }
}

/// Only named semantic metadata is copied. Unknown fields never enter the wire payload.
fn map_event(engine: &str, notify: bool, input: &Value) -> Option<Map<String, Value>> {
    input.as_object()?;
    let name = if notify {
        field(input, "type", 64)?
    } else {
        field(input, "hook_event_name", 64)?
    };
    let kind = match (engine, name) {
        ("codex", "agent-turn-complete") if notify => "turn.complete",
        (_, "SessionStart") => "session.start",
        (_, "SessionEnd") => "session.end",
        (_, "UserPromptSubmit") => "turn.start",
        (_, "PreToolUse") => "tool.start",
        (_, "PostToolUse") => "tool.finish",
        ("claude", "PostToolUseFailure")
            if input.get("is_interrupt") == Some(&Value::Bool(true)) =>
        {
            "turn.interrupted"
        }
        ("claude", "PostToolUseFailure") => "tool.finish",
        (_, "Stop") => "turn.stopped",
        ("claude", "StopFailure") => "turn.error",
        ("codex", "Interrupt") => "turn.interrupted",
        (_, "PermissionRequest") | ("claude", "Elicitation") => "approval.requested",
        ("claude", "PermissionDenied" | "ElicitationResult") => "approval.resolved",
        ("claude", "Notification")
            if matches!(
                field(input, "notification_type", 64),
                Some("permission_prompt" | "elicitation_prompt")
            ) =>
        {
            "approval.requested"
        }
        (_, "SubagentStart") => "subagent.start",
        (_, "SubagentStop") => "subagent.end",
        _ => return None,
    };
    let session_key = if notify { "thread-id" } else { "session_id" };
    let session = field(input, session_key, 256)?;
    let mut output = Map::new();
    for (key, value) in [
        ("engine", engine),
        ("client", "terminal"),
        ("session_id", session),
        ("kind", kind),
        ("origin", "hook"),
        ("source_event", name),
    ] {
        output.insert(key.into(), value.into());
    }
    output.insert("schema".into(), 2.into());
    copy_field(&mut output, "agent_id", input, "agent_id", 256);
    if matches!(kind, "subagent.start" | "subagent.end") && !output.contains_key("agent_id") {
        return None;
    }
    let turn_key = if notify {
        "turn-id"
    } else if engine == "claude" {
        "prompt_id"
    } else {
        "turn_id"
    };
    copy_field(&mut output, "turn_id", input, turn_key, 256);
    // Keep source=compact: a SessionStart hook after compaction is not a new idle run.
    if name == "SessionStart" {
        copy_field(&mut output, "source", input, "source", 32);
    }
    if name == "SessionEnd" {
        copy_field(&mut output, "reason", input, "reason", 64);
    }
    if matches!(name, "Stop" | "SubagentStop") {
        output.insert("tentative".into(), true.into());
    }
    if name == "PostToolUseFailure" {
        output.insert("outcome".into(), "error".into());
    }
    copy_field(&mut output, "tool_use_id", input, "tool_use_id", 256);
    copy_field(&mut output, "request_id", input, "request_id", 256);
    if !output.contains_key("request_id") {
        copy_field(&mut output, "request_id", input, "elicitation_id", 256);
    }
    if let Some(tool) = field(input, "tool_name", 128) {
        output.insert("detail".into(), tool.into());
    }
    if let Some(cwd) = field(input, "cwd", 4096).filter(|cwd| Path::new(cwd).is_absolute()) {
        output.insert("cwd".into(), cwd.into());
        if let Some(title) = Path::new(cwd)
            .file_name()
            .and_then(OsStr::to_str)
            .filter(|s| s.len() <= 256)
        {
            output.insert("title".into(), title.into());
        }
    }
    Some(output)
}

#[derive(Debug, PartialEq)]
struct Process {
    pid: u32,
    parent: u32,
    start_time: u64,
}

fn parse_stat(pid: u32, text: &str) -> Option<Process> {
    let (prefix, tail) = text.rsplit_once(')')?;
    if prefix.split_once(' ')?.0.parse::<u32>().ok()? != pid {
        return None;
    }
    let fields: Vec<_> = tail.split_whitespace().take(20).collect();
    Some(Process {
        pid,
        parent: fields.get(1)?.parse().ok()?,
        start_time: fields.get(19)?.parse().ok()?,
    })
}

fn process(pid: u32) -> Option<Process> {
    let directory = PathBuf::from(format!("/proc/{pid}"));
    if std::fs::metadata(&directory).ok()?.uid() != unsafe { libc::geteuid() } {
        return None;
    }
    let mut stat = String::new();
    std::fs::File::open(directory.join("stat"))
        .ok()?
        .take(4097)
        .read_to_string(&mut stat)
        .ok()?;
    if stat.len() > 4096 {
        return None;
    }
    parse_stat(pid, &stat)
}

fn find_owner(engine: &str) -> Option<Process> {
    let mut pid = unsafe { libc::getppid() } as u32;
    for _ in 0..16 {
        if pid <= 1 {
            return None;
        }
        let found = process(pid)?;
        let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
        // Resolve /proc/exe, not argv[0] or the shell/mise wrapper's display name.
        let name = exe.file_name()?.to_str()?.trim_end_matches(" (deleted)");
        if name == engine {
            return process(pid).filter(|current| current.start_time == found.start_time);
        }
        if found.parent == pid {
            return None;
        }
        pid = found.parent;
    }
    None
}

fn terminal_metadata(owner: &Process) -> Map<String, Value> {
    let mut result = Map::new();
    if let Ok(path) = std::fs::read_link(format!("/proc/{}/fd/0", owner.pid)) {
        if let Some(tty) = path
            .to_str()
            .filter(|s| s.starts_with("/dev/pts/") || s.starts_with("/dev/tty"))
        {
            if tty.len() <= 256 && !tty.chars().any(char::is_control) {
                result.insert("tty".into(), tty.into());
            }
        }
    }
    if let (Ok(tmux), Ok(pane)) = (std::env::var("TMUX"), std::env::var("TMUX_PANE")) {
        // TMUX may contain commas in the socket path; remove its final two fields.
        if let Some((rest, index)) = tmux.rsplit_once(',') {
            if let Some((socket, server_pid)) = rest.rsplit_once(',') {
                if index.parse::<u32>().is_ok()
                    && server_pid.parse::<u32>().is_ok()
                    && Path::new(socket).is_absolute()
                    && socket.len() <= 4096
                    && !socket.chars().any(char::is_control)
                    && pane
                        .strip_prefix('%')
                        .is_some_and(|n| !n.is_empty() && n.parse::<u32>().is_ok())
                {
                    result.insert("tmux_socket".into(), socket.into());
                    result.insert("tmux_pane".into(), pane.into());
                }
            }
        }
    }
    result
}

fn private_directory(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|m| m.is_dir() && m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o077 == 0)
}

fn send_event(runtime: &Path, data: &[u8]) -> io::Result<()> {
    if !runtime.is_absolute() || !private_directory(runtime) {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let directory = runtime.join("chonk-agents");
    if !private_directory(&directory) {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let path = directory.join("events.sock");
    let metadata = std::fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_socket() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let bytes = path.as_os_str().as_bytes();
    if bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    address.sun_family = libc::AF_UNIX as _;
    for (target, source) in address.sun_path.iter_mut().zip(bytes) {
        *target = *source as _;
    }
    let raw = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let socket = unsafe { OwnedFd::from_raw_fd(raw) };
    if unsafe {
        libc::connect(
            socket.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            std::mem::size_of_val(&address) as _,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let written = unsafe {
        libc::send(
            socket.as_raw_fd(),
            data.as_ptr().cast(),
            data.len(),
            libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
        )
    };
    if written != data.len() as isize {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_and_provider_specific_turn_identity() {
        let base = json!({"session_id":"native", "turn_id":"codex-turn", "prompt_id":"claude-prompt", "hook_event_name":"Stop"});
        for (engine, turn) in [("codex", "codex-turn"), ("claude", "claude-prompt")] {
            let event = map_event(engine, false, &base).unwrap();
            assert_eq!(event["kind"], "turn.stopped");
            assert_eq!(event["turn_id"], turn);
            assert_eq!(event["tentative"], true);
        }
        let event = map_event(
            "codex",
            false,
            &json!({"session_id":"native", "hook_event_name":"SessionStart", "source":"compact"}),
        )
        .unwrap();
        assert_eq!(event["source"], "compact");
        let event = map_event(
            "claude",
            false,
            &json!({"session_id":"native", "hook_event_name":"SessionEnd", "reason":"resume"}),
        )
        .unwrap();
        assert_eq!(event["kind"], "session.end");
        assert_eq!(event["reason"], "resume");
    }

    #[test]
    fn metadata_whitelist_drops_conversation_and_command_content() {
        let input = json!({"session_id":"native", "hook_event_name":"PreToolUse", "tool_name":"Bash", "cwd":"/work/project", "tool_input":{"command":"PRIVATE COMMAND"}, "prompt":"PRIVATE PROMPT", "last_assistant_message":"PRIVATE ANSWER", "transcript_path":"/private/history", "title":"PRIVATE TITLE"});
        let event = map_event("claude", false, &input).unwrap();
        assert_eq!(event["title"], "project");
        assert_eq!(event["detail"], "Bash");
        let text = serde_json::to_string(&event).unwrap();
        assert!(!text.contains("PRIVATE"));
        assert!(!text.contains("history"));
    }

    #[test]
    fn tool_failure_is_not_session_failure_and_permission_is_tentative() {
        let mut input = json!({"session_id":"s", "hook_event_name":"PostToolUseFailure"});
        assert_eq!(
            map_event("claude", false, &input).unwrap()["kind"],
            "tool.finish"
        );
        input["is_interrupt"] = true.into();
        assert_eq!(
            map_event("claude", false, &input).unwrap()["kind"],
            "turn.interrupted"
        );
        input["hook_event_name"] = "PermissionRequest".into();
        assert_eq!(
            map_event("codex", false, &input).unwrap()["kind"],
            "approval.requested"
        );
        assert_eq!(
            map_event("claude", false, &input).unwrap()["kind"],
            "approval.requested"
        );
        input["hook_event_name"] = "Interrupt".into();
        assert!(map_event("claude", false, &input).is_none());
    }

    #[test]
    fn subagent_identity_is_separate_and_notify_is_authoritative() {
        let mut input =
            json!({"session_id":"parent", "agent_id":"child", "hook_event_name":"SubagentStop"});
        let event = map_event("codex", false, &input).unwrap();
        assert_eq!(event["session_id"], "parent");
        assert_eq!(event["agent_id"], "child");
        assert_eq!(event["tentative"], true);
        input.as_object_mut().unwrap().remove("agent_id");
        assert!(map_event("codex", false, &input).is_none());
        let event = map_event("codex", true, &json!({"type":"agent-turn-complete", "thread-id":"native", "turn-id":"turn", "last-assistant-message":"SECRET"})).unwrap();
        assert_eq!(event["kind"], "turn.complete");
        assert_eq!(event["turn_id"], "turn");
        assert!(!event.contains_key("tentative"));
    }

    #[test]
    fn malformed_identity_and_delayed_idle_notifications_do_not_create_events() {
        for value in [
            Value::Null,
            json!([]),
            json!({"session_id":17,"hook_event_name":"Stop"}),
            json!({"session_id":"x\nspoof","hook_event_name":"Stop"}),
            json!({"session_id":"s","hook_event_name":"Unknown"}),
            json!({"session_id":"s","hook_event_name":"Notification","notification_type":"idle_prompt"}),
        ] {
            assert!(map_event("claude", false, &value).is_none());
        }
    }

    #[test]
    fn proc_stat_uses_final_parenthesis_and_exact_start_tick_field() {
        let mut fields = vec!["S", "42"];
        fields.extend(std::iter::repeat_n("0", 17));
        fields.push("123456");
        let text = format!("100 (strange ) process) {}", fields.join(" "));
        assert_eq!(
            parse_stat(100, &text),
            Some(Process {
                pid: 100,
                parent: 42,
                start_time: 123456
            })
        );
        assert!(parse_stat(101, &text).is_none());
        assert!(parse_stat(100, "100 (short) S 42").is_none());
    }
}
