//! One volatile clipboard snapshot, restored only when its owner disappears.
//! PRIMARY and clipboard history remain separate. All I/O is nonblocking,
//! bounded, and serviced with an explicit byte budget outside keyboard routing.

use crate::state::Compositor;
use smithay::wayland::selection::data_device::{current_data_device_selection_userdata, set_data_device_selection};
use smithay::wayland::selection::{SelectionSource, SelectionTarget};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::net::UnixStream,
    },
    sync::{Arc, Weak},
    time::{Duration, Instant},
};
use wm_core::Backend;

type Payloads = Arc<BTreeMap<String, Arc<Vec<u8>>>>;
const LIMIT: usize = 64 * 1024 * 1024;
const BUDGET: usize = 256 * 1024;
const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Default)]
pub enum SelectionData {
    #[default]
    Bridge,
    Memory(Weak<BTreeMap<String, Arc<Vec<u8>>>>),
}

struct ReadOffer {
    mime: String,
    stream: UnixStream,
    bytes: Vec<u8>,
}
struct WriteOffer {
    file: File,
    bytes: Arc<Vec<u8>>,
    offset: usize,
    deadline: Instant,
}
struct Snapshot {
    source: Option<SelectionSource>,
    x11_live: bool,
    pending: Vec<ReadOffer>,
    data: BTreeMap<String, Arc<Vec<u8>>>,
    bytes: usize,
    deadline: Instant,
}

#[derive(Default)]
pub(crate) struct Persistence {
    snapshot: Option<Snapshot>,
    current: Option<Payloads>,
    writes: Vec<WriteOffer>,
}

impl Persistence {
    pub(crate) fn active(&self) -> bool {
        !self.writes.is_empty() || self.snapshot.as_ref().is_some_and(|s| !s.pending.is_empty())
    }

    pub(crate) fn clear(&mut self) {
        self.snapshot = None;
        self.current = None;
    }

    pub(crate) fn x11_lost(&mut self) {
        if let Some(snapshot) = self.snapshot.as_mut().filter(|s| s.source.is_none()) {
            snapshot.x11_live = false;
        }
    }

    pub(crate) fn begin(&mut self, source: Option<SelectionSource>, mimes: Vec<String>) -> Vec<(String, OwnedFd)> {
        self.snapshot = None;
        // Respect the established confidential-data hint and manager opt-outs.
        // Never retain an old copy when a new offer cannot be persisted.
        if mimes.is_empty()
            || mimes.len() > 32
            || mimes
                .iter()
                .any(|m| m == "x-kde-passwordManagerHint" || m == "application/x-nopersist" || m.len() > 1024)
        {
            return Vec::new();
        }
        tracing::debug!(
            native = source.is_some(),
            formats = mimes.len(),
            "caching clipboard offer"
        );
        let mut snapshot = Snapshot {
            x11_live: source.is_none(),
            source,
            pending: Vec::new(),
            data: BTreeMap::new(),
            bytes: 0,
            deadline: Instant::now() + TIMEOUT,
        };
        let mut requests = Vec::new();
        for mime in mimes {
            if snapshot.pending.iter().any(|p| p.mime == mime) {
                continue;
            }
            let Ok((read, write)) = UnixStream::pair() else {
                return Vec::new();
            };
            if read.set_nonblocking(true).is_err() {
                return Vec::new();
            }
            snapshot.pending.push(ReadOffer {
                mime: mime.clone(),
                stream: read,
                bytes: Vec::new(),
            });
            requests.push((mime, write.into()));
        }
        self.snapshot = Some(snapshot);
        requests
    }

    pub(crate) fn send(&mut self, data: &SelectionData, mime: &str, fd: OwnedFd) -> bool {
        let SelectionData::Memory(data) = data else {
            return false;
        };
        if self.writes.len() >= 16 {
            return true;
        }
        let Some(data) = data.upgrade() else {
            return true;
        };
        let Some(bytes) = data.get(mime) else {
            return true;
        };
        let file = File::from(fd);
        // SAFETY: fcntl receives a live owned descriptor; no pointer arguments.
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        if flags < 0 {
            return true;
        }
        // SAFETY: same live descriptor as F_GETFL, with integer flag arguments.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return true;
        }
        self.writes.push(WriteOffer {
            file,
            bytes: bytes.clone(),
            offset: 0,
            deadline: Instant::now() + TIMEOUT,
        });
        true
    }
}

pub(crate) fn tick(comp: &mut Compositor) {
    poll(comp);
    let now = Instant::now();
    let quitting = comp.wm.backend().pending_quit.iter().any(|(_, at)| *at <= now);
    if quitting
        && !comp
            .clipboard_persistence
            .snapshot
            .as_ref()
            .is_some_and(|s| !s.pending.is_empty())
    {
        if let Some(snapshot) = comp.clipboard_persistence.snapshot.take() {
            publish(comp, snapshot.data);
        }
        let pending = std::mem::take(&mut comp.wm.backend_mut().pending_quit);
        for (id, at) in pending {
            if at <= now {
                comp.wm.backend_mut().send_close(id);
            } else {
                comp.wm.backend_mut().pending_quit.push((id, at));
            }
        }
    }
}

fn publish(comp: &mut Compositor, data: BTreeMap<String, Arc<Vec<u8>>>) {
    let mimes: Vec<_> = data.keys().cloned().collect();
    let data = Arc::new(data);
    let weak = Arc::downgrade(&data);
    comp.clipboard_persistence.current = Some(data);
    // Stale client offers must not retain every old clipboard generation.
    // In-flight sends retain only their representation until their deadline.
    set_data_device_selection(
        &comp.display_handle,
        &comp.seat,
        mimes.clone(),
        SelectionData::Memory(weak),
    );
    if let Some(xwm) = comp.xwayland.wm.as_mut() {
        if let Err(error) = xwm.new_selection(SelectionTarget::Clipboard, Some(mimes)) {
            tracing::warn!(?error, "could not publish persistent clipboard to XWayland");
        }
    }
}

fn poll(comp: &mut Compositor) {
    let now = Instant::now();
    let persistence = &mut comp.clipboard_persistence;
    let mut budget = BUDGET;
    persistence.writes.retain_mut(|offer| {
        if now >= offer.deadline {
            return false;
        }
        while offer.offset < offer.bytes.len() && budget > 0 {
            let end = offer.bytes.len().min(offer.offset + budget);
            match offer.file.write(&offer.bytes[offer.offset..end]) {
                Ok(0) => return false,
                Ok(n) => {
                    offer.offset += n;
                    budget -= n;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return false,
            }
        }
        offer.offset < offer.bytes.len()
    });
    if !comp.wm.mac_mode() || !comp.wm.interaction_config().clipboard_persistence {
        persistence.snapshot = None;
        return;
    }
    let Some(snapshot) = persistence.snapshot.as_mut() else {
        return;
    };
    if !snapshot.pending.is_empty() && now >= snapshot.deadline {
        persistence.clear();
        tracing::warn!("clipboard persistence timed out; original offer retained");
        return;
    }
    let mut failed = false;
    let mut buffer = [0u8; 16384];
    snapshot.pending.retain_mut(|offer| {
        while budget > 0 && !failed {
            let capacity = buffer.len().min(budget);
            match offer.stream.read(&mut buffer[..capacity]) {
                Ok(0) => {
                    tracing::debug!(bytes = offer.bytes.len(), "cached clipboard representation");
                    snapshot
                        .data
                        .insert(offer.mime.clone(), Arc::new(std::mem::take(&mut offer.bytes)));
                    return false;
                }
                Ok(n) => {
                    snapshot.bytes += n;
                    budget -= n;
                    if snapshot.bytes > LIMIT {
                        failed = true;
                        break;
                    }
                    offer.bytes.extend_from_slice(&buffer[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    failed = true;
                    break;
                }
            }
        }
        true
    });
    if failed {
        persistence.clear();
        tracing::warn!("clipboard offer could not be persisted within resource limits; original offer retained");
        return;
    }
    let lost = snapshot
        .source
        .as_ref()
        .map_or(!snapshot.x11_live, |source| !source.is_alive());
    if lost && snapshot.pending.is_empty() {
        let snapshot = persistence.snapshot.take().unwrap();
        tracing::debug!(
            bytes = snapshot.bytes,
            formats = snapshot.data.len(),
            "restoring clipboard after owner exit"
        );
        // A replacement compositor offer (for example a restarted bridge)
        // wins. Native/data-control replacements already cancelled our cache.
        if current_data_device_selection_userdata(&comp.seat).is_some() {
            return;
        }
        publish(comp, snapshot.data);
    }
}
