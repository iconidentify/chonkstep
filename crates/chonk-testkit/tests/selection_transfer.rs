//! Exact clipboard/primary payloads through ordinary focused Wayland clients
//! and the XWayland selection bridge. No connection targets the live desktop.

#[path = "support/x_selection.rs"]
mod x_selection;

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use chonk_testkit::{poll_until, profile_binary, Session, SessionOptions};

const EVENT: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug)]
enum Kind {
    Clipboard,
    Primary,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Clipboard => "clipboard",
            Self::Primary => "primary",
        }
    }
    fn copy_key(self) -> u32 {
        match self {
            Self::Clipboard => 63,
            Self::Primary => 64,
        }
    }
    fn paste_key(self) -> u32 {
        match self {
            Self::Clipboard => 65,
            Self::Primary => 66,
        }
    }
    fn clear_key(self) -> u32 {
        match self {
            Self::Clipboard => 67,
            Self::Primary => 68,
        }
    }
}

fn boot(name: &str) -> Session {
    Session::boot(
        name,
        SessionOptions {
            config_extra: format!("show_dock = false\nomarchy_menu = false\nhyprland_config = false\n{}",
                if name.starts_with("mac-") { "interaction_mode = 'mac'\n" } else { "" }),
            env: if std::env::var_os("CHONKSTEP_SELECTION_TRACE").is_some() {
                vec![(
                    "RUST_LOG".into(),
                    "info,smithay::xwayland::xwm=trace,wm_wayland::xwayland=trace".into(),
                )]
            } else {
                Vec::new()
            },
            ..Default::default()
        },
    )
    .unwrap()
}

fn request(session: &Session, command: &str) -> String {
    let socket = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap())
        .join("hypr")
        .join(session.hyprland_signature().unwrap())
        .join(".socket.sock");
    let mut stream = UnixStream::connect(socket).unwrap();
    stream.set_read_timeout(Some(EVENT)).unwrap();
    stream.set_write_timeout(Some(EVENT)).unwrap();
    stream.write_all(command.as_bytes()).unwrap();
    let mut response = String::new();
    stream
        .take(1024 * 1024)
        .read_to_string(&mut response)
        .unwrap();
    response
}

struct Native {
    name: String,
    directory: PathBuf,
    receives: usize,
}

impl Native {
    fn launch(session: &mut Session, name: &str, mime: &str, payload: &[u8]) -> Self {
        Self::launch_with_receive_mode(session, name, mime, payload, None)
    }

    fn launch_with_receive_mode(
        session: &mut Session,
        name: &str,
        mime: &str,
        payload: &[u8],
        mode: Option<&str>,
    ) -> Self {
        let directory = session.dir.join(name);
        std::fs::create_dir(&directory).unwrap();
        let binary = directory.join(name);
        // A distinct executable name makes each owner independently killable
        // and keeps its log identity stable through replacement/exit tests.
        std::os::unix::fs::symlink(profile_binary("chonk-selection-probe").unwrap(), &binary)
            .unwrap();
        let input = directory.join("input.bin");
        std::fs::write(&input, payload).unwrap();
        let mut args = vec![
            name,
            mime,
            input.to_str().unwrap(),
            directory.to_str().unwrap(),
        ];
        args.extend(mode);
        session
            .launch_isolated(binary.to_str().unwrap(), &args)
            .unwrap();
        session.wait_for_window(name).unwrap();
        let client = Self {
            name: name.into(),
            directory,
            receives: 0,
        };
        client.focus(session);
        client
    }

    fn log(&self, session: &Session) -> String {
        session.client_log(&self.name)
    }

    fn wait(&self, session: &Session, description: &str, mut ready: impl FnMut(&str) -> bool) {
        poll_until(EVENT, description, || {
            ready(&self.log(session)).then_some(())
        })
        .unwrap_or_else(|error| panic!("{error}\n{}\n{}", self.log(session), session.log()));
    }

    fn focus(&self, session: &mut Session) {
        let clients: serde_json::Value =
            serde_json::from_str(&request(session, "j/clients")).unwrap();
        let address = clients
            .as_array()
            .unwrap()
            .iter()
            .find(|client| client["class"] == self.name)
            .and_then(|client| client["address"].as_str())
            .expect("mapped native client address");
        assert_eq!(
            request(session, &format!("/dispatch focuswindow address:{address}")),
            "ok"
        );
        self.wait(session, "native client keyboard focus", |log| {
            log.lines().rev().find(|line| line.starts_with("keyboard ")) == Some("keyboard enter")
        });
    }

    fn owns_offer(&self, session: &Session, kind: Kind) -> bool {
        let prefix = format!("{} offer ", kind.name());
        self.log(session)
            .lines()
            .rev()
            .find_map(|line| line.strip_prefix(&prefix))
            == Some("true")
    }

    fn copy(&self, session: &mut Session, kind: Kind) {
        self.focus(session);
        let marker = format!("{} offer true", kind.name());
        let before = self
            .log(session)
            .lines()
            .filter(|line| *line == marker)
            .count();
        session.door().tap_key(kind.copy_key()).unwrap();
        self.wait(session, "the new native selection offer", |log| {
            log.lines().filter(|line| *line == marker).count() > before
        });
    }

    fn receive(&mut self, session: &mut Session, kind: Kind, payload: &[u8]) {
        self.receive_with(session, kind, payload, || {});
    }

    fn receive_with(
        &mut self,
        session: &mut Session,
        kind: Kind,
        payload: &[u8],
        mut pump: impl FnMut(),
    ) {
        self.focus(session);
        poll_until(EVENT, "the destination's selection offer", || {
            pump();
            self.owns_offer(session, kind).then_some(())
        })
        .unwrap();
        self.receives += 1;
        let path = self
            .directory
            .join(format!("{}-{}.bin", kind.name(), self.receives));
        session.door().tap_key(kind.paste_key()).unwrap();
        let received = poll_until(EVENT, "the complete native payload", || {
            pump();
            let bytes = std::fs::read(&path).ok()?;
            let marker = format!("received {} {}", kind.name(), bytes.len());
            self.log(session)
                .lines()
                .any(|line| line == marker)
                .then_some(bytes)
        })
        .unwrap_or_else(|error| panic!("{error}\n{}", self.log(session)));
        assert_eq!(received.len(), payload.len(), "{kind:?} length");
        assert!(received == payload, "{kind:?} payload bytes differ");
        assert!(session.compositor_alive());
    }
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn slow_native_consumers_receive_every_x11_incr_byte_in_both_selections() {
    let mut session = boot("selection-slow-native");
    let _ = session.connect_x11().unwrap();
    let mut native = Native::launch_with_receive_mode(
        &mut session,
        "selection-slow-native-client",
        "application/octet-stream",
        b"unused",
        Some("slow"),
    );
    let payload: Vec<_> = (0u8..=255).cycle().take(1024 * 1024 + 17).collect();
    let mut x11 = x_selection::XSelection::new(&mut session, "application/octet-stream", &payload);
    for kind in [Kind::Clipboard, Kind::Primary] {
        x11.copy(matches!(kind, Kind::Primary));
        native.receive_with(&mut session, kind, &payload, || x11.tick());
    }
    assert_eq!(x11.sent_incremental, 2);
}

fn start_gated_native_receive(
    session: &mut Session,
    native: &mut Native,
    x11: &mut x_selection::XSelection,
    kind: Kind,
) {
    x11.copy(matches!(kind, Kind::Primary));
    native.focus(session);
    poll_until(EVENT, "the held consumer's offer", || {
        x11.tick();
        native.owns_offer(session, kind).then_some(())
    })
    .unwrap();
    native.receives += 1;
    session.door().tap_key(kind.paste_key()).unwrap();
    let ready = format!("receive ready {} {}", kind.name(), native.receives);
    poll_until(
        EVENT,
        "the X11 source to send a chunk to the held consumer",
        || {
            x11.tick();
            let (streams, bytes) = x11.outgoing_progress();
            (streams == 1 && bytes >= 64 * 1024 && native.log(session).contains(&ready))
                .then_some(())
        },
    )
    .unwrap();
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn closing_native_consumers_retires_both_x11_incr_streams() {
    let mut session = boot("selection-cancel-native-consumer");
    let _ = session.connect_x11().unwrap();
    let mut native = Native::launch_with_receive_mode(
        &mut session,
        "selection-cancel-native-client",
        "application/octet-stream",
        b"unused",
        Some("gated"),
    );
    let payload: Vec<_> = (0u8..=255).cycle().take(1024 * 1024 + 17).collect();
    let mut x11 = x_selection::XSelection::new(&mut session, "application/octet-stream", &payload);
    for kind in [Kind::Clipboard, Kind::Primary] {
        start_gated_native_receive(&mut session, &mut native, &mut x11, kind);
        std::fs::write(
            native
                .directory
                .join(format!("receive-{}.cancel", native.receives)),
            b"cancel",
        )
        .unwrap();
        poll_until(
            EVENT,
            "the cancelled consumer to retire its X11 requestor",
            || {
                x11.tick();
                (x11.outgoing_progress().0 == 0).then_some(())
            },
        )
        .unwrap_or_else(|error| panic!("{error}\n{}\n{}", native.log(&session), session.log()));
        session.door().barrier().unwrap();
    }
    assert_eq!(x11.sent_incremental, 2);
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn a_disconnected_x11_incr_owner_closes_the_native_consumers_pipe() {
    let mut session = boot("selection-disconnected-x11-source");
    let _ = session.connect_x11().unwrap();
    let mut native = Native::launch_with_receive_mode(
        &mut session,
        "selection-disconnected-source-client",
        "application/octet-stream",
        b"unused",
        Some("gated"),
    );
    let payload: Vec<_> = (0u8..=255).cycle().take(1024 * 1024 + 17).collect();
    for kind in [Kind::Clipboard, Kind::Primary] {
        let mut x11 =
            x_selection::XSelection::new(&mut session, "application/octet-stream", &payload);
        start_gated_native_receive(&mut session, &mut native, &mut x11, kind);
        drop(x11);
        std::fs::write(
            native
                .directory
                .join(format!("receive-{}.resume", native.receives)),
            b"resume",
        )
        .unwrap();
        let prefix = format!("received {} ", kind.name());
        native.wait(
            &session,
            "EOF when the X11 INCR producer disappears",
            |log| log.lines().any(|line| line.starts_with(&prefix)),
        );
        let bytes = std::fs::read(native.directory.join(format!(
            "{}-{}.bin",
            kind.name(),
            native.receives
        )))
        .unwrap();
        assert!(
            bytes.len() < payload.len(),
            "the source must exit before it could finish the transfer"
        );
        assert_eq!(
            &bytes,
            &payload[..bytes.len()],
            "any delivered prefix is exact"
        );
        session.door().barrier().unwrap();
    }
}

fn bridge_roundtrip(name: &str, mime: &str, payload: &[u8], large: bool) {
    let mut session = boot(name);
    // This matrix measures steady-state transfer. Early native ownership while
    // XWayland is still starting needs its own controlled-startup regression.
    let _ = session.connect_x11().unwrap();
    let mut native = Native::launch(&mut session, "selection-native", mime, payload);
    for kind in [Kind::Clipboard, Kind::Primary] {
        native.copy(&mut session, kind);
    }
    let replacement = [payload, b"\nx11-owner"].concat();
    let mut x11 = x_selection::XSelection::new(&mut session, mime, &replacement);
    for kind in [Kind::Clipboard, Kind::Primary] {
        x11.receive(matches!(kind, Kind::Primary), payload);
    }
    for kind in [Kind::Clipboard, Kind::Primary] {
        x11.copy(matches!(kind, Kind::Primary));
        native.focus(&mut session);
        poll_until(
            EVENT,
            "X11 TARGETS to replace the native owner's offer",
            || {
                x11.tick();
                let log = native.log(&session);
                let marker = format!("{} cancelled\n", kind.name());
                let after = log.rsplit_once(&marker)?.1;
                after
                    .lines()
                    .any(|line| line == format!("{} offer true", kind.name()))
                    .then_some(())
            },
        )
        .unwrap();
        native.receive_with(&mut session, kind, &replacement, || x11.tick());
    }
    if large {
        assert_eq!(
            x11.received_incremental, 2,
            "both Wayland-to-X11 transfers used INCR"
        );
        assert_eq!(
            x11.sent_incremental, 2,
            "both X11-to-Wayland transfers used INCR"
        );
    }
    assert!(session.compositor_alive());
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn x11_wayland_clipboard_and_primary_preserve_utf8_in_both_directions() {
    bridge_roundtrip(
        "selection-bridge-utf8",
        "text/plain;charset=utf-8",
        "X11 ↔ Wayland — 日本語 😀\n".as_bytes(),
        false,
    );
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn x11_wayland_clipboard_and_primary_preserve_binary_in_both_directions() {
    let bytes: Vec<_> = (0u8..=255).cycle().take(4097).collect();
    bridge_roundtrip(
        "selection-bridge-binary",
        "application/octet-stream",
        &bytes,
        false,
    );
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn x11_wayland_clipboard_and_primary_use_incr_for_large_transfers() {
    let bytes: Vec<_> = (0u8..=255).cycle().take(1024 * 1024 + 17).collect();
    bridge_roundtrip(
        "selection-bridge-large",
        "application/octet-stream",
        &bytes,
        true,
    );
}

#[derive(Clone, Copy)]
enum EarlyOwner {
    Live,
    Cleared,
    Exited,
}

fn early_xwayland(name: &str, owner: EarlyOwner) {
    let original_path = std::env::var_os("PATH").unwrap();
    let real = std::env::split_paths(&original_path)
        .map(|directory| directory.join("Xwayland"))
        .find(|path| path.is_file())
        .expect("installed XWayland");
    let gate = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap())
        .join(format!("chonk-xwayland-gate-{}-{name}", std::process::id()));
    std::fs::create_dir(&gate).unwrap();
    std::os::unix::fs::symlink(
        profile_binary("chonk-xwayland-gate").unwrap(),
        gate.join("Xwayland"),
    )
    .unwrap();
    std::os::unix::fs::symlink(real, gate.join("real-Xwayland")).unwrap();
    let path = std::env::join_paths(
        std::iter::once(gate.clone()).chain(std::env::split_paths(&original_path)),
    )
    .unwrap();
    let mut session = Session::boot(
        name,
        SessionOptions {
            config_extra: "show_dock = false\nomarchy_menu = false\nhyprland_config = false\n"
                .into(),
            env: vec![("PATH".into(), path.into_string().unwrap())],
            ..Default::default()
        },
    )
    .unwrap();
    poll_until(EVENT, "the private XWayland startup gate", || {
        gate.join("started").exists().then_some(())
    })
    .unwrap();
    let payload = b"native owner predates XWayland readiness";
    let native = Native::launch(
        &mut session,
        "selection-early-native",
        "text/plain;charset=utf-8",
        payload,
    );
    for kind in [Kind::Clipboard, Kind::Primary] {
        native.copy(&mut session, kind);
    }
    assert!(
        !session.log().contains("XWayland ready"),
        "the intended startup order is enforced"
    );
    match owner {
        EarlyOwner::Live => {}
        EarlyOwner::Cleared => {
            for kind in [Kind::Clipboard, Kind::Primary] {
                session.door().tap_key(kind.clear_key()).unwrap();
                native.wait(&session, "the early native selection to clear", |log| {
                    let prefix = format!("{} offer ", kind.name());
                    log.lines()
                        .rev()
                        .find_map(|line| line.strip_prefix(&prefix))
                        == Some("false")
                });
            }
        }
        EarlyOwner::Exited => {
            session.kill_client(&native.name);
            session.wait_for_window_gone(&native.name).unwrap();
        }
    }
    std::fs::write(gate.join("release"), b"go").unwrap();
    let mut x11 = x_selection::XSelection::new(&mut session, "text/plain;charset=utf-8", b"unused");
    for primary in [false, true] {
        if matches!(owner, EarlyOwner::Live) {
            x11.receive(primary, payload);
        } else {
            assert!(
                !x11.has_owner(primary),
                "dead or cleared native selection was resurrected"
            );
        }
    }
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn native_selection_published_before_xwayland_ready_reaches_x11() {
    early_xwayland("selection-early-xwayland", EarlyOwner::Live);
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn a_cleared_early_selection_is_not_resurrected_when_xwayland_starts() {
    early_xwayland("selection-early-cleared", EarlyOwner::Cleared);
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn an_exited_early_owner_is_not_resurrected_when_xwayland_starts() {
    early_xwayland("selection-early-exited", EarlyOwner::Exited);
}

fn anonymous_kib(session: &Session) -> u64 {
    let data = std::fs::read_to_string(format!("/proc/{}/smaps_rollup", session.compositor_pid()))
        .unwrap();
    data.lines()
        .find_map(|line| {
            line.strip_prefix("Pss_Anon:")?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
        .unwrap()
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn a_stalled_x11_paste_applies_backpressure_instead_of_buffering_the_whole_payload() {
    let mut session = boot("selection-x11-backpressure");
    let _ = session.connect_x11().unwrap();
    let payload: Vec<_> = (0u8..=255).cycle().take(8 * 1024 * 1024 + 17).collect();
    let native = Native::launch(
        &mut session,
        "selection-pressure-source",
        "application/octet-stream",
        &payload,
    );
    native.copy(&mut session, Kind::Clipboard);
    let mut x11 = x_selection::XSelection::new(&mut session, "application/octet-stream", b"unused");
    let before = anonymous_kib(&session);
    x11.hold_receive(false);
    let completed = format!("send {} Ok(())", payload.len());
    let prematurely_drained = poll_until(
        Duration::from_millis(500),
        "a stalled sender to remain backpressured",
        || {
            x11.tick();
            native
                .log(&session)
                .lines()
                .any(|line| line == completed)
                .then_some(())
        },
    )
    .is_ok();
    let held = anonymous_kib(&session);
    std::fs::write(
        session.dir.join("backpressure.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "payload_bytes":payload.len(), "anonymous_before_kib":before,
            "anonymous_held_kib":held, "sender_completed_before_consumer":prematurely_drained,
        }))
        .unwrap(),
    )
    .unwrap();
    // Complete the same transfer even on the unfixed binary, preserving both
    // retention and payload evidence before reporting the failed policy.
    x11.resume_receive(&payload);
    session.door().barrier().unwrap();
    assert!(
        !prematurely_drained,
        "the compositor drained an 8 MiB source while its X11 consumer acknowledged no chunks"
    );
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn a_cancelled_x11_paste_unblocks_the_source_and_allows_the_next_paste() {
    let mut session = boot("selection-x11-cancelled");
    let _ = session.connect_x11().unwrap();
    let payload: Vec<_> = (0u8..=255).cycle().take(2 * 1024 * 1024 + 17).collect();
    let native = Native::launch(
        &mut session,
        "selection-cancel-source",
        "application/octet-stream",
        &payload,
    );
    native.copy(&mut session, Kind::Clipboard);
    let mut x11 = x_selection::XSelection::new(&mut session, "application/octet-stream", b"unused");
    x11.hold_receive(false);
    drop(x11);
    native.wait(
        &session,
        "a vanished X11 consumer to close the sender's pipe",
        |log| {
            log.lines().any(|line| {
                line.starts_with(&format!("send {} Err(", payload.len()))
                    && line.contains("BrokenPipe")
            })
        },
    );
    native.copy(&mut session, Kind::Clipboard);
    let mut replacement =
        x_selection::XSelection::new(&mut session, "application/octet-stream", b"unused");
    replacement.receive(false, &payload);
    assert_eq!(replacement.received_incremental, 1);
    session.door().barrier().unwrap();
}

fn wait_broken_sends(native: &Native, session: &Session, bytes: usize, count: usize) {
    native.wait(
        session,
        "every abandoned transfer to close its source pipe",
        |log| {
            log.lines()
                .filter(|line| {
                    line.starts_with(&format!("send {bytes} Err(")) && line.contains("BrokenPipe")
                })
                .count()
                == count
        },
    );
}

fn kill_private_xwayland(session: &Session) {
    let parent = session.compositor_pid();
    let children =
        std::fs::read_to_string(format!("/proc/{parent}/task/{parent}/children")).unwrap();
    let pid: i32 = children
        .split_whitespace()
        .find_map(|pid| {
            let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
            comm.trim()
                .eq_ignore_ascii_case("Xwayland")
                .then(|| pid.parse().unwrap())
        })
        .expect("only this nested compositor's direct XWayland child");
    // SAFETY: resolved from this live private compositor's direct child list.
    // No ambient display or unrelated X server is signalled.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
}

fn wait_restarted_xwayland(session: &Session) {
    poll_until(EVENT, "exactly one ready replacement XWayland", || {
        (session
            .log()
            .lines()
            .filter(|line| line.contains("XWayland ready"))
            .count()
            == 2)
            .then_some(())
    })
    .unwrap();
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn cancelling_one_requestor_retires_both_clipboard_and_primary_transfers() {
    let mut session = boot("selection-x11-cancel-both");
    let _ = session.connect_x11().unwrap();
    let payload: Vec<_> = (0u8..=255).cycle().take(2 * 1024 * 1024 + 17).collect();
    let native = Native::launch(
        &mut session,
        "selection-cancel-both-source",
        "application/octet-stream",
        &payload,
    );
    for kind in [Kind::Clipboard, Kind::Primary] {
        native.copy(&mut session, kind);
    }
    let mut x11 = x_selection::XSelection::new(&mut session, "application/octet-stream", b"unused");
    x11.assert_key_delivery(&mut session, 30);
    x11.hold_both_on_child();
    x11.cancel_child();
    wait_broken_sends(&native, &session, payload.len(), 2);
    let mut replacement =
        x_selection::XSelection::new(&mut session, "application/octet-stream", b"unused");
    replacement.assert_key_delivery(&mut session, 30);
    for primary in [false, true] {
        replacement.receive(primary, &payload);
    }
    session.door().barrier().unwrap();
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn xwayland_loss_cancels_pending_pastes_and_restores_the_native_owner() {
    let mut session = boot("selection-x11-restart-pending");
    let _ = session.connect_x11().unwrap();
    let payload: Vec<_> = (0u8..=255).cycle().take(2 * 1024 * 1024 + 17).collect();
    let native = Native::launch(
        &mut session,
        "selection-restart-source",
        "application/octet-stream",
        &payload,
    );
    for kind in [Kind::Clipboard, Kind::Primary] {
        native.copy(&mut session, kind);
    }
    let mut x11 = x_selection::XSelection::new(&mut session, "application/octet-stream", b"unused");
    x11.assert_key_delivery(&mut session, 30);
    x11.hold_both_on_child();
    kill_private_xwayland(&session);
    drop(x11);
    wait_broken_sends(&native, &session, payload.len(), 2);
    wait_restarted_xwayland(&session);
    // No second copy: ownership must survive in the authoritative native seat
    // and be exported when the replacement XWM becomes ready.
    let mut replacement =
        x_selection::XSelection::new(&mut session, "application/octet-stream", b"unused");
    // XGetInputFocus alone cannot prove that the compositor associated this
    // window with a live surface from the replacement XWayland generation.
    replacement.assert_key_delivery(&mut session, 30);
    for primary in [false, true] {
        replacement.receive(primary, &payload);
    }
    assert!(session.compositor_alive());
    assert!(!session
        .log()
        .contains("freed before being removed from EventLoop"));
    session.door().barrier().unwrap();
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn xwayland_loss_retires_offers_owned_by_the_dead_x11_generation() {
    let mut session = boot("selection-x11-restart-offer");
    let _ = session.connect_x11().unwrap();
    let mut x11 = x_selection::XSelection::new(
        &mut session,
        "application/octet-stream",
        b"vanished X11 clipboard",
    );
    let native = Native::launch(
        &mut session,
        "selection-observer",
        "application/octet-stream",
        b"unused native payload",
    );
    for kind in [Kind::Clipboard, Kind::Primary] {
        x11.copy(matches!(kind, Kind::Primary));
        poll_until(EVENT, "the X11 selection's offer to arrive", || {
            x11.tick();
            native.owns_offer(&session, kind).then_some(())
        })
        .unwrap();
    }
    kill_private_xwayland(&session);
    drop(x11);
    wait_restarted_xwayland(&session);
    native.focus(&mut session);
    native.wait(&session, "both dead X11 offers to be withdrawn", |log| {
        [Kind::Clipboard, Kind::Primary].into_iter().all(|kind| {
            log.lines()
                .rev()
                .find_map(|line| line.strip_prefix(&format!("{} offer ", kind.name())))
                == Some("false")
        })
    });
    assert!(session.compositor_alive());
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn native_selection_access_is_denied_without_x11_keyboard_focus() {
    let mut session = boot("selection-x11-focus-access");
    let _ = session.connect_x11().unwrap();
    let native = Native::launch(
        &mut session,
        "selection-private-source",
        "application/octet-stream",
        b"focused paste only",
    );
    for kind in [Kind::Clipboard, Kind::Primary] {
        native.copy(&mut session, kind);
    }
    let mut x11 = x_selection::XSelection::new(&mut session, "application/octet-stream", b"unused");
    native.focus(&mut session);
    for primary in [false, true] {
        x11.assert_refused_without_focus(primary);
    }
    for primary in [false, true] {
        x11.receive(primary, b"focused paste only");
    }
    session.door().barrier().unwrap();
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn completed_small_pastes_do_not_retain_a_buffer_per_live_requestor() {
    let mut session = boot("selection-x11-completed-retention");
    let _ = session.connect_x11().unwrap();
    let payload: Vec<_> = (0u8..=255).cycle().take(60 * 1024).collect();
    let native = Native::launch(
        &mut session,
        "selection-completed-source",
        "application/octet-stream",
        &payload,
    );
    native.copy(&mut session, Kind::Clipboard);
    let mut x11 = x_selection::XSelection::new(&mut session, "application/octet-stream", b"unused");
    // Warm the ordinary path before sampling. Leave each subsequent requestor
    // alive: completed transfer state must not depend on its eventual death.
    x11.receive(false, &payload);
    session.door().barrier().unwrap();
    let before = anonymous_kib(&session);
    for _ in 0..128 {
        x11.new_requestor_child();
        x11.receive(false, &payload);
    }
    session.door().barrier().unwrap();
    let after = anonymous_kib(&session);
    std::fs::write(
        session.dir.join("completed-retention.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "payload_bytes": payload.len(), "requestors": 128,
            "anonymous_before_kib": before, "anonymous_after_kib": after,
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(x11.received_incremental, 0);
    assert!(
        after.saturating_sub(before) < 4 * 1024,
        "completed pastes retained {} KiB with requestors still alive",
        after.saturating_sub(before)
    );
}

fn native_roundtrip(name: &str, mime: &str, payload: &[u8]) {
    let mut session = boot(name);
    let replacement = [payload, b"\nreplacement-owner"].concat();
    let mut source = Native::launch(&mut session, "selection-source", mime, payload);
    for kind in [Kind::Clipboard, Kind::Primary] {
        source.copy(&mut session, kind);
    }
    let mut sink = Native::launch(&mut session, "selection-sink", mime, &replacement);
    for kind in [Kind::Clipboard, Kind::Primary] {
        sink.receive(&mut session, kind, payload);
    }
    for kind in [Kind::Clipboard, Kind::Primary] {
        sink.copy(&mut session, kind);
        source.wait(&session, "old selection owner cancellation", |log| {
            log.contains(&format!("{} cancelled", kind.name()))
        });
        source.receive(&mut session, kind, &replacement);
    }
    // Clearing PRIMARY must not clear CLIPBOARD, and neither operation may
    // leave the focused destination holding an offer to the vanished source.
    source.focus(&mut session);
    session.door().tap_key(Kind::Primary.clear_key()).unwrap();
    source.wait(&session, "primary cleared", |log| {
        log.lines()
            .rev()
            .find(|line| line.starts_with("primary offer "))
            == Some("primary offer false")
    });
    source.receive(&mut session, Kind::Clipboard, &replacement);
    session.door().tap_key(Kind::Clipboard.clear_key()).unwrap();
    source.wait(&session, "clipboard cleared", |log| {
        log.lines()
            .rev()
            .find(|line| line.starts_with("clipboard offer "))
            == Some("clipboard offer false")
    });
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn native_clipboard_and_primary_preserve_utf8_and_replace_owners() {
    native_roundtrip(
        "selection-native-utf8",
        "text/plain;charset=utf-8",
        "ChonkStep — café\n日本語 😀\n".as_bytes(),
    );
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn native_clipboard_and_primary_preserve_binary_and_embedded_nuls() {
    let bytes: Vec<_> = (0u8..=255).cycle().take(4097).collect();
    native_roundtrip(
        "selection-native-binary",
        "application/octet-stream",
        &bytes,
    );
}

#[test]
#[ignore = "needs a nested session; scripts/e2e.sh --headless --release"]
fn native_clipboard_and_primary_transfer_one_mib_without_truncation() {
    let bytes: Vec<_> = (0u8..=255).cycle().take(1024 * 1024 + 17).collect();
    native_roundtrip("selection-native-large", "application/octet-stream", &bytes);
}


#[test]
#[ignore = "scripts/e2e.sh --headless --test selection_transfer mac_persistence"]
fn mac_persistence_preserves_rich_payloads_after_the_source_quits() {
    let mut session = boot("mac-persistence-formats");
    session.screenshot("clipboard-fixture").unwrap();
    let png=std::fs::read(session.dir.join("01-clipboard-fixture.png")).unwrap();
    let formats = [
        ("text/html", "<p><b>café</b> 日本語 🍎</p>".as_bytes().to_vec()),
        ("text/uri-list", b"file:///tmp/clipboard%20fixture.txt\r\n".to_vec()),
        ("image/png", png),
        ("application/octet-stream", (0u8..=255).cycle().take(1024*1024+17).collect()),
    ];
    for (index, (mime, payload)) in formats.into_iter().enumerate() {
        let source = Native::launch(&mut session,&format!("mac-source-{index}"),mime,&payload);
        source.copy(&mut session,Kind::Clipboard);
        // The normal application Quit route finishes outstanding reads before
        // the client exits. The destination is an ordinary unprivileged client.
        session.door().chord(125,16).unwrap();
        session.wait_for_window_gone(&source.name).unwrap();
        let mut destination=Native::launch(&mut session,&format!("mac-destination-{index}"),mime,b"unused");
        destination.receive(&mut session,Kind::Clipboard,&payload);
        session.kill_client(&destination.name);
    }
}

fn set_mac_mode(session: &mut Session, enabled: bool) {
    let reloads = session.log().matches("reload requested").count();
    session.rewrite_config(&format!(
        "interaction_mode='{}'\nhyprland_config=false\nshow_dock=false\nomarchy_shell=false\n",
        if enabled { "mac" } else { "desktop" }
    )).unwrap();
    session.request_reload().unwrap();
    poll_until(EVENT, "interaction mode reload", || {
        (session.log().matches("reload requested").count() > reloads).then_some(())
    }).unwrap();
    session.door().barrier().unwrap();
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test selection_transfer enabling_mac"]
fn enabling_mac_preserves_an_existing_native_clipboard_owner() {
    let mut session = boot("selection-toggle-native");
    for cycle in 0..3 {
        set_mac_mode(&mut session, false);
        let payload = format!("before Mac enable {cycle}: café 日本語 🍎");
        let source = Native::launch(&mut session, &format!("toggle-source-{cycle}"),
            "text/plain;charset=utf-8", payload.as_bytes());
        source.copy(&mut session, Kind::Clipboard);
        set_mac_mode(&mut session, true);
        session.door().chord(125, 16).unwrap();
        session.wait_for_window_gone(&source.name).unwrap();
        let mut destination = Native::launch(&mut session, &format!("toggle-target-{cycle}"),
            "text/plain;charset=utf-8", b"unused");
        destination.receive(&mut session, Kind::Clipboard, payload.as_bytes());
        session.kill_client(&destination.name);
    }
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test selection_transfer enabling_mac"]
fn enabling_mac_preserves_an_existing_x11_clipboard_owner() {
    let mut session = boot("selection-toggle-x11");
    let payload = b"X11 clipboard copied before enabling Mac mode";
    let mut source = x_selection::XSelection::new(&mut session, "text/plain;charset=utf-8", payload);
    source.copy(false);
    let mut destination = Native::launch(&mut session, "toggle-x11-target",
        "text/plain;charset=utf-8", b"unused");
    destination.receive_with(&mut session, Kind::Clipboard, payload, || source.tick());
    set_mac_mode(&mut session, true);
    // Pump the real owner's protocol queue before disconnecting it. A mode
    // transition must request the existing offer without requiring another copy.
    let until = std::time::Instant::now() + Duration::from_millis(750);
    while std::time::Instant::now() < until {
        source.tick();
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(source);
    session.wait_for_window_gone("selection-x11").unwrap();
    destination.receive(&mut session, Kind::Clipboard, payload);
}
