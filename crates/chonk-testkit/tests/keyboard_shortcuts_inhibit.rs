//! `zwp_keyboard_shortcuts_inhibit_v1` policy over the real wire: who is
//! granted the desktop's chords, the chord that takes them back, and
//! what outranks all of it.
//!
//! The observable for "the desktop owns the chord" is a `run` binding
//! that appends to a marker file, as the XWayland grab test uses; the
//! observable for "the client owns it" is the probe's own key log. A
//! plain key tapped after the chord under test, and waited for in that
//! log, is the wire-order barrier the negative assertions stand on.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chonk_testkit::{poll_until, profile_binary, session_dir, Session, SessionOptions, WindowInfo};
use wayland_client::protocol::{wl_callback, wl_registry};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};
use wayland_protocols::wp::security_context::v1::client::{
    wp_security_context_manager_v1, wp_security_context_v1,
};

const EVENT: Duration = Duration::from_secs(10);
const PROBE: &str = "chonk-input-probe";
const KEY_ESC: u32 = 1;
const KEY_R: u32 = 19;
const KEY_A: u32 = 30;
const KEY_LEFTSHIFT: u32 = 42;
const KEY_F6: u32 = 64;
const KEY_LEFTMETA: u32 = 125;

/// A session whose Super+R appends a line to the returned marker, so
/// "the desktop ran its binding" is a line count. `extra` is top-level
/// config, `root_files` the isolated `XDG_CONFIG_HOME` (a Hyprland
/// configuration, for the posture that reads one).
fn boot(name: &str, extra: &str, root_files: &[(&str, &str)]) -> (Session, PathBuf) {
    let marker = session_dir(name).join("pressed");
    let config = format!(
        "omarchy_menu = false\nshow_dock = false\n{extra}\n[commands]\nmark = [\"sh\", \"-c\", \"echo ran >> {}\"]\n\n[keybindings]\n\"super+r\" = \"run mark\"\n",
        marker.display()
    );
    let session = Session::boot(
        name,
        SessionOptions {
            config_extra: config,
            config_root_files: root_files.iter().map(|(path, text)| (path.to_string(), text.to_string())).collect(),
            ..SessionOptions::default()
        },
    )
    .unwrap();
    (session, marker)
}

/// Launches the probe with an inhibitor on its toplevel and focuses it.
fn probe(session: &mut Session, extra_args: &[&str]) -> WindowInfo {
    let binary = profile_binary(PROBE).expect("input probe built");
    let mut args = vec!["1", "inhibit"];
    args.extend_from_slice(extra_args);
    session.launch(binary.to_str().unwrap(), &args).expect("probe launches");
    let window = session.wait_for_window("input-probe").expect("probe maps");
    // Focus lands on map; the click is for the sessions where something
    // else holds it, and the wait is for the first enter either way.
    session.door().click(f64::from(window.x + 80), f64::from(window.y + 70)).unwrap();
    wait_lines(session, "keyboard enter", 1);
    window
}

fn focus(session: &mut Session, window: &WindowInfo) {
    let enters = count(session, "keyboard enter");
    session.door().click(f64::from(window.x + 80), f64::from(window.y + 70)).unwrap();
    wait_lines(session, "keyboard enter", enters + 1);
}

fn count(session: &Session, line: &str) -> usize {
    session.client_log(PROBE).lines().filter(|value| *value == line).count()
}

fn wait_lines(session: &Session, line: &str, at_least: usize) {
    poll_until(EVENT, line, || (count(session, line) >= at_least).then_some(()))
        .unwrap_or_else(|error| panic!("{error}\n{}", session.client_log(PROBE)));
}

fn wait_log(session: &Session, needle: &str) {
    poll_until(EVENT, needle, || session.log().contains(needle).then_some(()))
        .unwrap_or_else(|error| panic!("{error}\n{}", session.log()));
}

fn ran(marker: &Path) -> usize {
    std::fs::read_to_string(marker).map_or(0, |text| text.lines().count())
}

fn wait_ran(marker: &Path, at_least: usize) {
    poll_until(EVENT, "the bound command to run", || (ran(marker) >= at_least).then_some(())).unwrap();
}

/// Super+R, the bound chord.
fn super_r(session: &mut Session) {
    session.door().chord(KEY_LEFTMETA, KEY_R).unwrap();
}

/// Super+Shift+Escape, the default escape chord, key by key.
fn escape_chord(session: &mut Session) {
    let door = session.door();
    door.key(KEY_LEFTMETA, true).unwrap();
    door.key(KEY_LEFTSHIFT, true).unwrap();
    door.barrier().unwrap();
    door.key(KEY_ESC, true).unwrap();
    door.barrier().unwrap();
    door.key(KEY_ESC, false).unwrap();
    door.key(KEY_LEFTSHIFT, false).unwrap();
    door.key(KEY_LEFTMETA, false).unwrap();
    door.barrier().unwrap();
}

/// A plain key the focused probe reports back: everything injected
/// before it has been delivered once its release is in the log.
fn settle(session: &mut Session) {
    let ups = count(session, "keyboard key 30 up");
    session.door().tap_key(KEY_A).unwrap();
    wait_lines(session, "keyboard key 30 up", ups + 1);
}

/// `hyprctl systeminfo`, plain.
fn systeminfo(session: &Session) -> String {
    let log = session.log();
    let directory = log
        .lines()
        .find(|line| line.contains("hyprland ipc listening"))
        .and_then(|line| line.split("directory=\"").nth(1)?.split('"').next())
        .expect("the compositor announces its Hyprland IPC directory")
        .to_string();
    let mut socket = UnixStream::connect(Path::new(&directory).join(".socket.sock")).unwrap();
    socket.set_read_timeout(Some(EVENT)).unwrap();
    socket.write_all(b"systeminfo").unwrap();
    socket.shutdown(std::net::Shutdown::Write).unwrap();
    let mut response = String::new();
    socket.read_to_string(&mut response).unwrap();
    response
}

fn assert_no_grant(session: &mut Session, marker: &Path, why: &str) {
    settle(session);
    assert_eq!(count(session, "shortcut-inhibitor active"), 0, "a declined inhibitor must never receive `active`");
    wait_log(session, "declined a keyboard-shortcuts inhibitor");
    assert!(session.log().contains(why), "the refusal names its reason: {why}\n{}", session.log());
    // The desktop still owns the chord, and the client never sees it.
    let before = ran(marker);
    super_r(session);
    wait_ran(marker, before + 1);
    settle(session);
    assert_eq!(count(session, "keyboard key 19 down"), 0, "an ungranted client must not receive a bound chord");
}

#[test]
#[ignore = "needs a session to nest in; run via scripts/e2e.sh"]
fn the_escape_chord_suspends_a_grant_across_recreation_and_focus_until_pressed_again() {
    let (mut session, marker) = boot("shortcuts-inhibit-escape", "", &[]);
    let window = probe(&mut session, &[]);
    wait_lines(&session, "shortcut-inhibitor active", 1);
    wait_log(&session, "keyboard shortcuts inhibited");
    assert!(
        systeminfo(&session).contains("shortcut_inhibitor: active holder=input-probe"),
        "{}",
        systeminfo(&session)
    );

    // Granted: the bound chord reaches the client, not the desktop.
    super_r(&mut session);
    wait_lines(&session, "keyboard key 19 up", 1);
    assert_eq!(ran(&marker), 0, "an inhibited chord must not run the desktop's binding");

    // The escape chord: the grant is withdrawn (`inactive`), the chord
    // is swallowed with its release, and the binding is the desktop's.
    escape_chord(&mut session);
    wait_lines(&session, "shortcut-inhibitor inactive", 1);
    wait_log(&session, "keyboard shortcuts restored by the user");
    super_r(&mut session);
    wait_ran(&marker, 1);
    settle(&mut session);
    assert_eq!(count(&session, "keyboard key 1 down"), 0, "the escape press must not reach the client");
    assert_eq!(count(&session, "keyboard key 1 up"), 0, "...nor its release");
    assert_eq!(count(&session, "keyboard key 19 down"), 1, "a suspended client must not receive the bound chord");
    assert!(
        systeminfo(&session).contains("shortcut_inhibitor: suspended holder=input-probe"),
        "{}",
        systeminfo(&session)
    );

    // Destroy and recreate: the suspension belongs to the surface, so
    // the new inhibitor is declined and the desktop keeps the chord.
    session.door().tap_key(KEY_F6).unwrap();
    wait_lines(&session, "shortcut-inhibitor recreated", 1);
    wait_log(&session, "declined a keyboard-shortcuts inhibitor");
    assert!(session.log().contains("the user suspended this window's grant"), "{}", session.log());
    super_r(&mut session);
    wait_ran(&marker, 2);
    settle(&mut session);
    assert_eq!(count(&session, "shortcut-inhibitor active"), 1, "recreating the inhibitor must not re-grant it");

    // A focus round trip — to another window and back — does not
    // re-grant it either.
    let other = profile_binary(PROBE).unwrap();
    session
        .launch("sh", &["-c", &format!("exec {} 1 --app-id=shortcuts-inhibit-other", other.display())])
        .unwrap();
    let other = session.wait_for_window("shortcuts-inhibit-other").unwrap();
    session.door().click(f64::from(other.x + other.w as i32 / 2), f64::from(other.y + other.h as i32 / 2)).unwrap();
    wait_lines(&session, "keyboard leave", 1);
    focus(&mut session, &window);
    super_r(&mut session);
    wait_ran(&marker, 3);
    settle(&mut session);
    assert_eq!(count(&session, "shortcut-inhibitor active"), 1, "a focus round trip must not re-grant a suspended window");

    // The second press resumes the grant: the client owns the chord again.
    escape_chord(&mut session);
    wait_lines(&session, "shortcut-inhibitor active", 2);
    wait_log(&session, "resumed the window's keyboard-shortcuts inhibitor");
    super_r(&mut session);
    wait_lines(&session, "keyboard key 19 up", 2);
    assert_eq!(ran(&marker), 3, "a resumed grant forwards the chord to the client");
    assert!(
        systeminfo(&session).contains("shortcut_inhibitor: active holder=input-probe"),
        "{}",
        systeminfo(&session)
    );
}

#[test]
#[ignore = "needs a session to nest in; run via scripts/e2e.sh"]
fn suspending_a_second_window_keeps_the_first_suspended() {
    let (mut session, marker) = boot("shortcuts-inhibit-two-suspended", "", &[]);
    let first = probe(&mut session, &[]);
    wait_lines(&session, "shortcut-inhibitor active", 1);
    escape_chord(&mut session);
    wait_lines(&session, "shortcut-inhibitor inactive", 1);

    let binary = profile_binary(PROBE).unwrap();
    session.launch("sh", &["-c", &format!(
        "exec {} 1 inhibit --app-id=second-inhibitor", binary.display()
    )]).unwrap();
    session.wait_for_window("second-inhibitor").unwrap();
    poll_until(EVENT, "second inhibitor active", || {
        systeminfo(&session).contains("active holder=second-inhibitor").then_some(())
    }).unwrap();
    escape_chord(&mut session);
    assert!(systeminfo(&session).contains("suspended holder=second-inhibitor"));

    focus(&mut session, &first);
    settle(&mut session);
    assert_eq!(count(&session, "shortcut-inhibitor active"), 1,
        "suspending a second surface must not erase the first surface's suspension");
    super_r(&mut session);
    wait_ran(&marker, 1);
    escape_chord(&mut session);
    wait_lines(&session, "shortcut-inhibitor active", 2);
}

#[test]
#[ignore = "needs a session to nest in; run via scripts/e2e.sh"]
fn allow_shortcut_inhibit_false_denies_every_grant() {
    let (mut session, marker) = boot("shortcuts-inhibit-off", "allow_shortcut_inhibit = false", &[]);
    probe(&mut session, &[]);
    assert_no_grant(&mut session, &marker, "allow_shortcut_inhibit is off");
    assert!(systeminfo(&session).contains("shortcut_inhibitor: disabled"), "{}", systeminfo(&session));
}

#[test]
#[ignore = "needs a session to nest in; run via scripts/e2e.sh"]
fn hyprlands_disable_keybind_grabbing_denies_every_grant_too() {
    let hyprland = "binds {\n    disable_keybind_grabbing = true\n}\nbind = SUPER, F12, workspace, 1\n";
    let (mut session, marker) = boot(
        "shortcuts-inhibit-hyprland-off",
        "desktop = \"omarchy\"\nomarchy_bar = false",
        &[("hypr/hyprland.conf", hyprland)],
    );
    probe(&mut session, &[]);
    assert_no_grant(&mut session, &marker, "allow_shortcut_inhibit is off");
    assert!(systeminfo(&session).contains("shortcut_inhibitor: disabled"), "{}", systeminfo(&session));
}

// ---------------------------------------------------------------------
// A client admitted through wp_security_context_v1.

#[derive(Default)]
struct Creator {
    globals: HashMap<String, (u32, u32)>,
    done: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Creator {
    fn event(
        state: &mut Self,
        _: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            state.globals.insert(interface, (name, version));
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for Creator {
    fn event(state: &mut Self, _: &wl_callback::WlCallback, _: wl_callback::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        state.done = true;
    }
}

macro_rules! ignore_events {
    ($($proxy:ty),* $(,)?) => {$(
        impl Dispatch<$proxy, ()> for Creator {
            fn event(_: &mut Self, _: &$proxy, _: <$proxy as wayland_client::Proxy>::Event,
                _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore_events!(
    wp_security_context_manager_v1::WpSecurityContextManagerV1,
    wp_security_context_v1::WpSecurityContextV1,
);

/// The ordinary client that opens the sandbox listener, kept alive
/// with it: the context lives as long as its close fd does.
struct Sandbox {
    _connection: Connection,
    _queue: EventQueue<Creator>,
    _listener: UnixListener,
    _keep_open: UnixStream,
}

fn sandbox(session: &Session, socket: &Path) -> Sandbox {
    let host = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap()).join(&session.wayland_display);
    let connection = Connection::from_socket(UnixStream::connect(host).unwrap()).unwrap();
    let mut queue = connection.new_event_queue();
    let registry = connection.display().get_registry(&queue.handle(), ());
    let mut state = Creator::default();
    connection.display().sync(&queue.handle(), ());
    connection.flush().unwrap();
    while !state.done {
        queue.blocking_dispatch(&mut state).unwrap();
    }
    let (name, _) = state.globals["wp_security_context_manager_v1"];
    let manager: wp_security_context_manager_v1::WpSecurityContextManagerV1 =
        registry.bind(name, 1, &queue.handle(), ());
    let listener = UnixListener::bind(socket).unwrap();
    let (close_fd, keep_open) = UnixStream::pair().unwrap();
    let context = manager.create_listener(listener.as_fd(), close_fd.as_fd(), &queue.handle(), ());
    context.set_sandbox_engine("shortcuts-inhibit-test".into());
    context.set_app_id("org.chonkstep.confined-inhibitor".into());
    context.commit();
    state.done = false;
    connection.display().sync(&queue.handle(), ());
    connection.flush().unwrap();
    while !state.done {
        queue.blocking_dispatch(&mut state).unwrap();
    }
    Sandbox { _connection: connection, _queue: queue, _listener: listener, _keep_open: keep_open }
}

#[test]
#[ignore = "needs a session to nest in; run via scripts/e2e.sh"]
fn a_sandboxed_client_is_never_granted_the_chords() {
    let (mut session, marker) = boot("shortcuts-inhibit-sandbox", "", &[]);
    let socket = session.dir.join("confined.sock");
    let _sandbox = sandbox(&session, &socket);
    // The manager global is visible to it — the protocol is
    // application-level — so the request goes through; the grant does not.
    let socket_arg = format!("--socket={}", socket.display());
    probe(&mut session, &[&socket_arg]);
    assert_no_grant(&mut session, &marker, "the client is sandboxed");
    assert!(systeminfo(&session).contains("shortcut_inhibitor: none"), "{}", systeminfo(&session));
}

#[test]
#[ignore = "needs a session to nest in; run via scripts/e2e.sh"]
fn the_session_lock_outranks_an_inhibiting_client_and_its_escape_chord() {
    let (mut session, marker) = boot("shortcuts-inhibit-lock", "", &[]);
    let window = probe(&mut session, &[]);
    wait_lines(&session, "shortcut-inhibitor active", 1);
    let locker = profile_binary("chonk-lock-probe").expect("lock probe built");
    session.launch(locker.to_str().unwrap(), &["--hold"]).unwrap();
    wait_log(&session, "session locking");
    // The lock surface takes the keyboard, and the grant goes with it.
    wait_lines(&session, "keyboard leave", 1);
    wait_lines(&session, "shortcut-inhibitor inactive", 1);
    poll_until(EVENT, "the lock surface to cover the probe", || {
        (session.door().hit(window.x + 80, window.y + 70).ok()? == "lock").then_some(())
    })
    .unwrap();
    // Under the lock every key is the locker's: neither the binding
    // nor the escape chord means anything, and the client sees none.
    let restored = session.log().matches("restored by the user").count();
    super_r(&mut session);
    escape_chord(&mut session);
    session.door().barrier().unwrap();
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(ran(&marker), 0, "a locked session must not run a desktop binding");
    assert_eq!(session.log().matches("restored by the user").count(), restored, "the escape chord is the locker's");
    assert!(!session.log().contains("resumed the window's"), "{}", session.log());
    assert_eq!(count(&session, "keyboard key 19 down"), 0, "the inhibited client must not receive keys under the lock");
}

// ---------------------------------------------------------------------
// zwp_xwayland_keyboard_grab_v1: the same chord releases an X11 grab.

fn map_x11_window(connection: &x11rb::rust_connection::RustConnection, screen: usize) -> u32 {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, CreateWindowAux, EventMask, PropMode, WindowClass};
    use x11rb::wrapper::ConnectionExt as _;
    let root = connection.setup().roots[screen].root;
    let xid = connection.generate_id().unwrap();
    connection
        .create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            xid,
            root,
            0,
            0,
            400,
            300,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new().background_pixel(0x406080).event_mask(EventMask::KEY_PRESS | EventMask::KEY_RELEASE),
        )
        .unwrap();
    connection
        .change_property8(PropMode::REPLACE, xid, AtomEnum::WM_NAME, AtomEnum::STRING, b"shortcuts-inhibit-x11")
        .unwrap();
    connection.map_window(xid).unwrap();
    connection.flush().unwrap();
    xid
}

#[test]
#[ignore = "needs a session to nest in; run via scripts/e2e.sh"]
fn the_escape_chord_releases_an_x11_keyboard_grab() {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{ConnectionExt, GrabMode};

    let (mut session, marker) = boot("shortcuts-inhibit-x11-grab", "", &[]);
    let (connection, screen) = session.connect_x11().unwrap();
    let xid = map_x11_window(&connection, screen);
    let window = poll_until(EVENT, "the X11 window to map", || {
        session.world().ok()?.windows.into_iter().find(|window| window.mapped)
    })
    .unwrap();
    session.door().click(f64::from(window.x + 80), f64::from(window.y + 70)).unwrap();
    poll_until(EVENT, "the X server to focus its window", || {
        (connection.get_input_focus().ok()?.reply().ok()?.focus == xid).then_some(())
    })
    .unwrap();
    connection
        .grab_keyboard(true, xid, x11rb::CURRENT_TIME, GrabMode::ASYNC, GrabMode::ASYNC)
        .unwrap()
        .reply()
        .expect("the X server grants the keyboard grab");
    connection.flush().unwrap();
    wait_log(&session, "an XWayland client took the keyboard grab");

    // Grabbed: the desktop's chord belongs to the X client.
    super_r(&mut session);
    session.door().barrier().unwrap();
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(ran(&marker), 0, "a grabbing X11 client receives the chord instead of the desktop");

    // The escape chord releases the grab, and the chord is the desktop's again.
    escape_chord(&mut session);
    wait_log(&session, "the XWayland keyboard grab was released");
    super_r(&mut session);
    wait_ran(&marker, 1);
}
