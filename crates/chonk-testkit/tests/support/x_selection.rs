//! Bounded ICCCM selection owner/requestor for the private XWayland display.
//! Large transfers implement the property-delete INCR handshake in both
//! directions; no xclip executable or connection to an ambient DISPLAY.

use std::collections::HashMap;
use std::time::Duration;

use chonk_testkit::{poll_until, Session};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ClientMessageEvent, ConnectionExt, CreateWindowAux,
    EventMask, PropMode, Property, SelectionNotifyEvent, SelectionRequestEvent, Window,
    WindowClass, SELECTION_NOTIFY_EVENT,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

const LIMIT: usize = 16 * 1024 * 1024;
const CHUNK: usize = 64 * 1024;
const EVENT: Duration = Duration::from_secs(15);

struct Outgoing {
    position: usize,
    target: Atom,
}

#[derive(Default)]
struct Incoming {
    incremental: bool,
    complete: bool,
    refused: bool,
    bytes: Vec<u8>,
}

pub struct XSelection {
    connection: RustConnection,
    root: Window,
    window: Window,
    requestor: Window,
    clipboard: Atom,
    targets: Atom,
    target: Atom,
    incr: Atom,
    property: Atom,
    payload: Vec<u8>,
    outgoing: HashMap<(Window, Atom), Outgoing>,
    incoming: Incoming,
    keys: Vec<(u8, bool)>,
    hold_incremental: bool,
    pub sent_incremental: usize,
    pub received_incremental: usize,
}

fn atom(connection: &RustConnection, name: &[u8]) -> Atom {
    connection
        .intern_atom(false, name)
        .unwrap()
        .reply()
        .unwrap()
        .atom
}

impl XSelection {
    pub fn new(session: &mut Session, mime: &str, payload: &[u8]) -> Self {
        let (connection, screen) = session.connect_x11().unwrap();
        let root = connection.setup().roots[screen].root;
        let window = connection.generate_id().unwrap();
        connection
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                window,
                root,
                0,
                0,
                320,
                200,
                0,
                WindowClass::INPUT_OUTPUT,
                x11rb::COPY_FROM_PARENT,
                &CreateWindowAux::new()
                    .background_pixel(0x307050)
                    .event_mask(
                        EventMask::PROPERTY_CHANGE
                            | EventMask::STRUCTURE_NOTIFY
                            | EventMask::KEY_PRESS
                            | EventMask::KEY_RELEASE,
                    ),
            )
            .unwrap();
        connection
            .change_property8(
                PropMode::REPLACE,
                window,
                AtomEnum::WM_NAME,
                AtomEnum::STRING,
                b"selection-x11",
            )
            .unwrap();
        connection.map_window(window).unwrap();
        connection.flush().unwrap();
        let mut client = Self {
            clipboard: atom(&connection, b"CLIPBOARD"),
            targets: atom(&connection, b"TARGETS"),
            // UTF8_STRING is the standard X11 text conversion target. Smithay
            // maps it to text/plain;charset=utf-8 on the native side.
            target: atom(
                &connection,
                if mime == "text/plain;charset=utf-8" {
                    b"UTF8_STRING"
                } else {
                    mime.as_bytes()
                },
            ),
            incr: atom(&connection, b"INCR"),
            property: atom(&connection, b"CHONK_TEST_SELECTION"),
            connection,
            root,
            window,
            requestor: window,
            payload: payload.to_vec(),
            outgoing: HashMap::new(),
            incoming: Incoming::default(),
            keys: Vec::new(),
            hold_incremental: false,
            sent_incremental: 0,
            received_incremental: 0,
        };
        client.focus();
        client
    }

    pub fn focus(&mut self) {
        let active = atom(&self.connection, b"_NET_ACTIVE_WINDOW");
        self.connection
            .send_event(
                false,
                self.root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                ClientMessageEvent::new(32, self.window, active, [2, x11rb::CURRENT_TIME, 0, 0, 0]),
            )
            .unwrap();
        self.connection.flush().unwrap();
        poll_until(
            EVENT,
            "the private X11 selection window to receive focus",
            || {
                self.tick();
                (self.connection.get_input_focus().ok()?.reply().ok()?.focus == self.window)
                    .then_some(())
            },
        )
        .unwrap();
    }

    pub fn assert_key_delivery(&mut self, session: &mut Session, evdev_key: u8) {
        self.tick();
        self.keys.clear();
        session.door().tap_key(u32::from(evdev_key)).unwrap();
        let expected = [(evdev_key + 8, true), (evdev_key + 8, false)];
        poll_until(
            EVENT,
            "the focused X11 client to receive the first key pair",
            || {
                self.tick();
                (self.keys == expected).then_some(())
            },
        )
        .unwrap_or_else(|error| {
            panic!(
                "{error}; observed {:?}; compositor log:\n{}",
                self.keys,
                session.log()
            )
        });
    }

    fn selection(&self, primary: bool) -> Atom {
        if primary {
            AtomEnum::PRIMARY.into()
        } else {
            self.clipboard
        }
    }

    pub fn has_owner(&self, primary: bool) -> bool {
        self.connection
            .get_selection_owner(self.selection(primary))
            .unwrap()
            .reply()
            .unwrap()
            .owner
            != x11rb::NONE
    }

    pub fn copy(&mut self, primary: bool) {
        let selection = self.selection(primary);
        self.connection
            .set_selection_owner(self.window, selection, x11rb::CURRENT_TIME)
            .unwrap();
        self.connection.flush().unwrap();
        assert_eq!(
            self.connection
                .get_selection_owner(selection)
                .unwrap()
                .reply()
                .unwrap()
                .owner,
            self.window
        );
    }

    pub fn receive(&mut self, primary: bool, expected: &[u8]) {
        self.begin_receive(primary, false);
        self.finish_receive(expected);
    }

    pub fn hold_receive(&mut self, primary: bool) {
        self.begin_receive(primary, true);
        poll_until(
            EVENT,
            "the X11 owner to announce INCR without consuming it",
            || {
                self.tick();
                assert!(!self.incoming.refused, "held conversion refused");
                self.incoming.incremental.then_some(())
            },
        )
        .unwrap();
        assert!(!self.incoming.complete);
    }

    pub fn resume_receive(&mut self, expected: &[u8]) {
        assert!(self.hold_incremental && self.incoming.incremental);
        self.hold_incremental = false;
        self.connection
            .delete_property(self.requestor, self.property)
            .unwrap();
        self.connection.flush().unwrap();
        self.finish_receive(expected);
    }

    fn begin_receive(&mut self, primary: bool, hold: bool) {
        self.focus();
        self.request_selection(primary, hold);
    }

    fn request_selection(&mut self, primary: bool, hold: bool) {
        self.incoming = Incoming::default();
        self.hold_incremental = hold;
        self.connection
            .convert_selection(
                self.requestor,
                self.selection(primary),
                self.target,
                self.property,
                x11rb::CURRENT_TIME,
            )
            .unwrap();
        self.connection.flush().unwrap();
    }

    pub fn assert_refused_without_focus(&mut self, primary: bool) {
        self.request_selection(primary, false);
        poll_until(EVENT, "the unfocused X11 request to be refused", || {
            self.tick();
            self.incoming.complete.then_some(())
        })
        .unwrap();
        assert!(
            self.incoming.refused,
            "unfocused XWayland read native clipboard data"
        );
        assert!(self.incoming.bytes.is_empty());
    }

    /// Keep two independent INCR conversions stalled on one unmapped child.
    /// Its destruction generates only the selected StructureNotify event,
    /// not a second root SubstructureNotify that could conceal partial cleanup.
    pub fn hold_both_on_child(&mut self) {
        assert_eq!(self.requestor, self.window);
        self.new_requestor_child();
        self.hold_receive(false);
        self.property = atom(&self.connection, b"CHONK_TEST_PRIMARY_HELD");
        self.hold_receive(true);
        assert_eq!(self.received_incremental, 2);
    }

    pub fn new_requestor_child(&mut self) {
        self.requestor = self.connection.generate_id().unwrap();
        self.connection
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                self.requestor,
                self.window,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_OUTPUT,
                x11rb::COPY_FROM_PARENT,
                &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
            )
            .unwrap();
    }

    pub fn cancel_child(&mut self) {
        assert_ne!(self.requestor, self.window);
        self.connection.destroy_window(self.requestor).unwrap();
        self.connection.flush().unwrap();
    }

    fn finish_receive(&mut self, expected: &[u8]) {
        poll_until(EVENT, "the X11 clipboard transfer to complete", || {
            self.tick();
            self.incoming.complete.then_some(())
        })
        .unwrap();
        assert!(
            !self.incoming.refused,
            "X11 conversion unexpectedly refused"
        );
        assert_eq!(
            self.incoming.bytes.len(),
            expected.len(),
            "X11 payload length"
        );
        assert!(self.incoming.bytes == expected, "X11 payload bytes differ");
    }

    pub fn tick(&mut self) {
        for _ in 0..128 {
            let Some(event) = self.connection.poll_for_event().unwrap() else {
                break;
            };
            match event {
                Event::KeyPress(event) if event.event == self.window => {
                    self.keys.push((event.detail, true));
                }
                Event::KeyRelease(event) if event.event == self.window => {
                    self.keys.push((event.detail, false));
                }
                Event::SelectionRequest(request) => self.send(request),
                Event::SelectionNotify(reply) if reply.requestor == self.requestor => {
                    if reply.property == x11rb::NONE {
                        self.incoming.refused = true;
                        self.incoming.complete = true;
                        continue;
                    }
                    assert_eq!(reply.property, self.property);
                    let data = self
                        .connection
                        .get_property(
                            !self.hold_incremental,
                            self.requestor,
                            self.property,
                            AtomEnum::ANY,
                            0,
                            (LIMIT / 4 + 1) as u32,
                        )
                        .unwrap()
                        .reply()
                        .unwrap();
                    assert_eq!(data.bytes_after, 0, "bounded initial selection property");
                    if data.type_ == self.incr {
                        self.incoming.incremental = true;
                        self.received_incremental += 1;
                    } else {
                        assert_eq!(data.type_, self.target);
                        assert_eq!(data.format, 8);
                        self.incoming.bytes = data.value;
                        self.incoming.complete = true;
                    }
                }
                Event::PropertyNotify(event) if event.state == Property::DELETE => {
                    let key = (event.window, event.atom);
                    if let Some(transfer) = self.outgoing.get_mut(&key) {
                        let end = (transfer.position + CHUNK).min(self.payload.len());
                        self.connection
                            .change_property8(
                                PropMode::REPLACE,
                                event.window,
                                event.atom,
                                transfer.target,
                                &self.payload[transfer.position..end],
                            )
                            .unwrap();
                        let finished = transfer.position == end;
                        transfer.position = end;
                        if finished {
                            self.outgoing.remove(&key);
                        }
                    }
                }
                Event::PropertyNotify(event)
                    if event.window == self.requestor
                        && event.atom == self.property
                        && event.state == Property::NEW_VALUE
                        && self.incoming.incremental
                        && !self.hold_incremental
                        && !self.incoming.complete =>
                {
                    let data = self
                        .connection
                        .get_property(
                            true,
                            self.requestor,
                            self.property,
                            AtomEnum::ANY,
                            0,
                            (LIMIT / 4 + 1) as u32,
                        )
                        .unwrap()
                        .reply()
                        .unwrap();
                    assert_eq!(data.bytes_after, 0, "bounded incremental chunk");
                    assert_eq!(data.type_, self.target);
                    assert_eq!(data.format, 8);
                    if data.value.is_empty() {
                        self.incoming.complete = true;
                    }
                    self.incoming.bytes.extend_from_slice(&data.value);
                    assert!(self.incoming.bytes.len() <= LIMIT, "bounded INCR payload");
                }
                Event::DestroyNotify(event) => self
                    .outgoing
                    .retain(|(window, _), _| *window != event.window),
                _ => {}
            }
        }
        self.connection.flush().unwrap();
    }

    pub fn outgoing_progress(&self) -> (usize, usize) {
        (
            self.outgoing.len(),
            self.outgoing
                .values()
                .map(|transfer| transfer.position)
                .sum(),
        )
    }

    fn send(&mut self, request: SelectionRequestEvent) {
        assert_eq!(request.owner, self.window);
        let property = if request.property == x11rb::NONE {
            request.target
        } else {
            request.property
        };
        let accepted = if request.target == self.targets {
            self.connection
                .change_property32(
                    PropMode::REPLACE,
                    request.requestor,
                    property,
                    AtomEnum::ATOM,
                    &[self.targets, self.target],
                )
                .unwrap();
            true
        } else if request.target == self.target {
            if self.payload.len() >= CHUNK {
                assert!(self.outgoing.len() < 16, "bounded test INCR streams");
                self.connection
                    .change_window_attributes(
                        request.requestor,
                        &ChangeWindowAttributesAux::new()
                            .event_mask(EventMask::PROPERTY_CHANGE | EventMask::STRUCTURE_NOTIFY),
                    )
                    .unwrap();
                self.connection
                    .change_property32(
                        PropMode::REPLACE,
                        request.requestor,
                        property,
                        self.incr,
                        &[self.payload.len() as u32],
                    )
                    .unwrap();
                assert!(self
                    .outgoing
                    .insert(
                        (request.requestor, property),
                        Outgoing {
                            position: 0,
                            target: request.target
                        }
                    )
                    .is_none());
                self.sent_incremental += 1;
            } else {
                self.connection
                    .change_property8(
                        PropMode::REPLACE,
                        request.requestor,
                        property,
                        request.target,
                        &self.payload,
                    )
                    .unwrap();
            }
            true
        } else {
            false
        };
        self.connection
            .send_event(
                false,
                request.requestor,
                EventMask::NO_EVENT,
                SelectionNotifyEvent {
                    response_type: SELECTION_NOTIFY_EVENT,
                    sequence: 0,
                    time: request.time,
                    requestor: request.requestor,
                    selection: request.selection,
                    target: request.target,
                    property: if accepted { property } else { x11rb::NONE },
                },
            )
            .unwrap();
    }
}
